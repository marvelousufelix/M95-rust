# Payout Readiness Implementation

## Overview

This document describes the implementation of payout readiness checking to prevent merchant balance loss when the Paystack payout provider account is unfunded.

## Problem Statement

The withdrawal endpoint was connected to Paystack but could fail because Aframp's Paystack balance was unfunded. Previously, the system would:

1. Debit the merchant's balance immediately
2. Attempt the payout to Paystack
3. On failure, refund the balance (compensating transaction)

This flow exposed merchants to a brief window where their balance appeared reduced even though no payout was possible. **The new implementation prevents this by checking payout readiness upfront, before any balance is debited.**

## Solution: Payout Readiness Signal

### Design Principles

1. **Pre-flight validation**: Check provider funding before debiting balance
2. **Early rejection**: Return 400 if payout is not ready, 502 if readiness check itself fails
3. **Zero balance loss**: Rejected requests do not create withdrawal records or debit balance
4. **Clear messaging**: Provide actionable error messages about funding status
5. **Atomic accounting**: Preserve existing transaction guarantees

## Implementation Details

### 1. PayoutReadiness Struct (`src/payments/mod.rs`)

```rust
pub struct PayoutReadiness {
    pub is_ready: bool,
    pub available_balance: Option<i64>,
    pub message: String,
}
```

Encapsulates the provider's funding status with a human-readable message and optional balance information for logging.

### 2. Extended PaymentProvider Trait (`src/payments/mod.rs`)

Added a new trait method:

```rust
pub trait PaymentProvider: Send + Sync {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String>;
    async fn create_payout(&self, req: &PayoutRequest) -> Result<PayoutResult, String>;
}
```

All implementations must now implement both methods.

### 3. PaystackProvider Implementation (`src/payments/paystack.rs`)

The `check_payout_readiness` method:

- Queries Paystack's `/balance` endpoint
- Returns `is_ready: true` if balance > 0
- Returns `is_ready: false` if balance <= 0
- Treats any readiness check error as a readiness failure (not a payout failure)
- Logs warnings but doesn't hard-fail on resolution errors

```rust
async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
    match self.get::<BalanceInfo>("/balance", &[]).await {
        Ok(balance_info) => {
            let is_ready = balance_info.balance > 0;
            // ... return PayoutReadiness
        }
        Err(err) => {
            // Readiness check failure → report as not ready
            Ok(PayoutReadiness {
                is_ready: false,
                available_balance: None,
                message: format!("Unable to verify Paystack account status: {err}"),
            })
        }
    }
}
```

### 4. MockProvider Update (`src/payments/mock.rs`)

For testing, the mock provider always returns ready status:

```rust
async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
    Ok(PayoutReadiness {
        is_ready: true,
        available_balance: Some(1_000_000_000),
        message: "Mock provider is always ready".into(),
    })
}
```

### 5. Withdrawal Service Logic (`src/services/withdrawals.rs`)

The critical change in `create_withdrawal`:

```rust
// Check payout readiness BEFORE debiting the balance
let readiness = provider
    .check_payout_readiness()
    .await
    .map_err(|e| WithdrawalError::PayoutFailed(format!("failed to check payout readiness: {e}")))?;

if !readiness.is_ready {
    return Err(WithdrawalError::PayoutNotReady(readiness.message));
}

// Only now proceed with balance debit and withdrawal creation
let amount_kobo = withdrawal.amount_stroops / STROOPS_PER_KOBO;
// ... transaction proceeds
```

**Key behavior:**
- Returns `PayoutNotReady` error **before** any balance is debited
- No withdrawal record is created
- Merchant balance remains unchanged
- Returns 400 status code

### 6. Error Types

#### WithdrawalError Enum (`src/services/withdrawals.rs`)

```rust
pub enum WithdrawalError {
    InsufficientBalance,
    UnsupportedAsset,
    InvalidAmountPrecision,
    PayoutNotReady(String),        // New variant
    PayoutFailed(String),
    Database(#[from] sqlx::Error),
}
```

#### ErrorCode Enum (`src/error.rs`)

```rust
pub enum ErrorCode {
    // ... existing codes
    PayoutNotReady,  // New
    PayoutFailed,
    // ...
}

// Serializes as "PAYOUT_NOT_READY"
```

### 7. API Error Response (`src/error.rs`)

Updated `ApiError` struct to include the error code:

```rust
pub struct ApiError {
    pub error: String,           // Human-readable message
    pub code: Option<String>,    // Machine-readable code (e.g., "PAYOUT_NOT_READY")
    pub field: Option<String>,   // Field name for validation errors
}
```

The error now has HTTP status codes:

- **400** (PayoutNotReady): Provider not ready - merchant can retry later when provider is funded
- **502** (PayoutFailed): Provider error during payout execution - may be transient

### 8. API Handler (`src/api/withdrawals.rs`)

Maps the new error variant:

```rust
fn map_withdrawal_error(err: WithdrawalError) -> (StatusCode, Json<ApiError>) {
    match err {
        WithdrawalError::PayoutNotReady(msg) => bad_request(
            ErrorCode::PayoutNotReady,
            &format!("payout not ready: {msg}"),
        ),
        // ... other cases
    }
}
```

## Testing

### Test Suite: `tests/payout_readiness_flow.rs`

Four comprehensive test scenarios:

#### 1. `withdrawal_succeeds_when_payout_ready()`

- Provider simulates Paystack with balance
- Merchant has sufficient balance
- Withdrawal should succeed
- Balance should be debited
- Withdrawal record should be created with `status: "pending"`

#### 2. `withdrawal_rejected_when_payout_not_ready()`

- Provider simulates Paystack with zero balance
- Merchant has sufficient balance
- Withdrawal should be rejected with 400 status
- Error code should be `PAYOUT_NOT_READY`
- **Merchant balance should NOT be debited** (critical assertion)
- No withdrawal record should be created

#### 3. `withdrawal_rejected_when_readiness_check_fails()`

- Provider readiness check returns error
- Merchant has sufficient balance
- Withdrawal should be rejected with 502 status (provider error)
- Error code should be `PAYOUT_FAILED`
- **Merchant balance should NOT be debited**
- No withdrawal record should be created

#### 4. `withdrawal_balance_preserved_across_ready_and_unready()`

- First successful withdrawal when provider is ready
- Balance is debited (3M of 10M available)
- Switch provider to unready
- Second withdrawal attempt is rejected
- **Balance should remain at 7M, not further debited**
- Only one withdrawal record exists (the successful one)

### Mock Providers

Three mock providers for testing different scenarios:

- **ReadyProvider**: Simulates funded Paystack account
- **UnreadyProvider**: Simulates unfunded Paystack account (zero balance)
- **CheckFailureProvider**: Simulates network/auth errors in readiness check

## Database

### Migration: `migrations/0007_payout_readiness_checks.sql`

Creates audit trail for payout readiness checks:

```sql
CREATE TABLE payout_readiness_checks (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  merchant_id UUID NOT NULL REFERENCES merchants(id),
  withdrawal_id UUID REFERENCES withdrawals(id),
  provider TEXT NOT NULL,
  is_ready BOOLEAN NOT NULL,
  available_balance BIGINT,
  message TEXT NOT NULL,
  checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_payout_readiness_withdrawal ON payout_readiness_checks(withdrawal_id);
CREATE INDEX idx_payout_readiness_merchant_time ON payout_readiness_checks(merchant_id, checked_at DESC);
```

**Note**: This table is created for audit/debugging purposes. The implementation validates readiness at withdrawal time without requiring entries in this table (to keep the critical path fast). The table can be populated by background monitoring if needed.

## API Documentation

### Updated `API.md`

#### Error Code Catalog

Added:

```
| `PAYOUT_NOT_READY` | `400` | Payout provider account does not have sufficient funding |
```

#### POST /withdraw Endpoint

New documentation section: **"Payout readiness check"**

Explains:

1. Readiness is verified before balance is debited
2. 400 + `PAYOUT_NOT_READY` means provider is unfunded - retry later
3. Balance is never lost
4. 502 + `PAYOUT_FAILED` means provider error after readiness passed - compensating refund occurs

#### New Operations Section: "Operations: Enabling payouts"

Describes:

1. Current state: Paystack not funded, withdrawals rejected upfront
2. How to enable: Fund Paystack account, test with small transfer
3. Expected behavior once funded: Readiness check passes, balance is debited, payout proceeds

## Acceptance Criteria - Verification

✅ **Unavailable funding source prevents withdrawal**: When Paystack balance ≤ 0, withdrawal request returns 400 with `PAYOUT_NOT_READY` before balance is debited

✅ **Rejected requests preserve balance**: No withdrawal record is created when request is rejected for unavailable funding. Merchant balance remains unchanged.

✅ **Test coverage**: Four test scenarios verify available, unavailable, and provider-failure paths

✅ **Withdrawal history shows consistent status**: Rejected withdrawals are not recorded. Only accepted/attempted withdrawals appear in history.

✅ **API documentation explains behavior**: API.md section "Payout readiness check" explains the pre-flight validation and "Operations: Enabling payouts" describes how to enable payouts

## Backwards Compatibility

✅ **No breaking changes to existing endpoints**:
- New error code added (400 status)
- New trait method added to PaymentProvider (requires implementation in all providers)
- ApiError response includes new optional `code` field (backwards compatible due to optional serialization)
- Withdrawal flow still succeeds when readiness passes

✅ **Existing test suite unaffected**:
- Existing withdrawal tests continue to work
- New tests added for readiness scenarios

## Future Enhancements

1. **Readiness table population**: Could populate `payout_readiness_checks` table for detailed audit trails
2. **Readiness caching**: Could cache readiness status per provider to reduce API calls
3. **Admin endpoint**: Could add `/admin/payout-readiness` endpoint to check provider status
4. **Alerts**: Could trigger alerts when Paystack balance drops below threshold
5. **Multiple providers**: Could support provider failover if one is unfunded

## Files Modified

1. `src/payments/mod.rs` - Added PayoutReadiness struct and trait method
2. `src/payments/paystack.rs` - Implemented readiness check via /balance endpoint
3. `src/payments/mock.rs` - Updated mock provider for trait
4. `src/services/withdrawals.rs` - Added readiness check before balance debit
5. `src/error.rs` - Added PayoutNotReady error code and updated ApiError struct
6. `src/api/withdrawals.rs` - Added error mapping for PayoutNotReady
7. `migrations/0007_payout_readiness_checks.sql` - Created audit table
8. `tests/payout_readiness_flow.rs` - New comprehensive test suite
9. `API.md` - Updated documentation with readiness behavior

## Deployment Notes

1. Run new migration `0007_payout_readiness_checks.sql` before deploying code
2. No configuration changes required - uses existing Paystack credentials
3. Existing withdrawals not affected - only future requests use readiness check
4. Once Paystack account is funded (balance > 0), readiness check passes and withdrawals proceed normally
5. No special monitoring required - error logs will show readiness check failures if they occur
