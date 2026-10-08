# Stellar Deposit Confirmation Policy — Implementation Summary

## Overview

Implemented a complete 32-ledger finality confirmation policy for Stellar deposits. Deposits now transition through `detected → verified → confirmed` with explicit ledger-based confirmation tracking before balances are credited to merchants.

## Key Decision: 32-Ledger Finality Threshold

**Policy:** Deposits are confirmed when the transaction has been included in at least **32 consecutive ledger sequences** beyond its confirmation ledger (approximately 160 seconds at normal Stellar network velocity of ~5s per ledger).

**Rationale:**
- Stellar's consensus is fast (~5s finality) with extremely low reorg risk compared to PoW networks
- However, "immediate confirmation" offers zero protection against ledger halts, consensus pauses, or validator set changes
- 32 ledgers (~160s) is conservative enough to catch any pathological scenario while fast enough for POS merchant experience
- Aligns with industry practice (Ethereum safe finality ~13 blocks/3 minutes; Solana 32 slots)

## Database Changes

### Migration 0008: `confirmation_threshold.sql`
- Added `confirmation_ledger` (BIGINT, nullable) — Stellar ledger sequence where transaction was confirmed
- Added `confirmation_threshold` (INT, default 32) — configurable finality requirement per payment
- Created index on (merchant_id, status, confirmation_ledger) for efficient pending confirmation queries

## Model & Service Layer Updates

### Payment Model (`src/models/payment.rs`)
```rust
pub struct Payment {
    // ... existing fields ...
    pub confirmation_ledger: Option<i64>,      // When was this confirmed on-chain
    pub confirmation_threshold: i32,            // How many ledgers needed for finality
}
```

### Payments Service (`src/services/payments.rs`)
**New functions:**
- `set_confirmation_ledger(id, ledger)` — Record the confirmation ledger when a payment is verified
- `ready_to_confirm(current_ledger)` → Vec<Uuid> — Find payments that have reached their threshold
- `pending_confirmations(current_ledger)` → Vec<(Uuid, i64, i32)> — Find payments still pending

## Blockchain Integration

### DetectedDeposit Struct (`src/blockchain/stellar.rs`)
```rust
pub struct DetectedDeposit {
    // ... existing fields ...
    pub confirmation_ledger: i64,  // Extracted from Horizon transaction data
}
```

**Horizon Integration:**
- Enhanced OperationRecord deserialization to capture `ledger_sequence` at both operation and transaction levels
- Parser extracts the confirmation ledger from transaction data in all operation types (payment, create_account, path_payment_strict_send, claimable_balance_created)

## Worker Implementation (`src/blockchain/worker.rs`)

**Two-Phase Polling Architecture:**

**Phase 1: Detect New Deposits**
1. Fetch all Stellar wallet addresses from database
2. Query Horizon for payments per address
3. For each detected deposit:
   - Record as `detected` status (no balance change)
   - Transition to `verified` status
   - Set `confirmation_ledger` from Horizon transaction data
   - Add amount to merchant's `pending` balance
   - Attempt memo-based correlation with payment requests

**Phase 2: Finalize Confirmed Deposits**
1. Fetch current Stellar ledger sequence from Horizon
2. Query for payments where `status = 'verified'` and `current_ledger - confirmation_ledger >= 32`
3. For each ready payment:
   - Transition to `confirmed` status
   - Move amount from `pending` → `available` balance

**Error Handling:** Each deposit and confirmation is processed independently; failures don't block other payments.

## Balance Ledger Behavior

| Deposit Status | Pending Balance | Available Balance |
|---|---|---|
| `detected` | ➖ 0 | ➖ 0 |
| `verified` (< 32 ledgers) | ✅ Credited | ➖ 0 |
| `confirmed` (≥ 32 ledgers) | ➖ 0 | ✅ Credited |

**Merchant UX:**
- Sees payment in `GET /transactions` immediately upon detection
- Sees balance in `GET /balance` under `pending` when verified on-chain
- Sees balance in `GET /balance` under `available` after 32 ledgers (~160s)
- Can see confirmation progress via `confirmation_ledger` and `confirmation_threshold` fields on transaction objects

## Test Coverage (`tests/deposit_confirmation_flow.rs`)

10 comprehensive tests covering:
1. **Detected status** — Payment starts as detected with no balance change
2. **Verified transitions** — Transitions set confirmation_ledger correctly
3. **Pending balance tracking** — Amounts move to pending on verification
4. **Insufficient confirmations** — Payments with < 32 ledgers stay verified
5. **Threshold validation** — Threshold check logic at exactly 32 and beyond
6. **Balance finalization** — Balance moves from pending → available on confirmation
7. **Multiple stages** — Different payments at different confirmation stages handled correctly
8. **No balance on detect** — Detected status doesn't affect balances
9. **Balance accuracy** — Single balance updates per status transition
10. **Edge cases** — Null/missing ledger fields handled correctly

All tests verify:
- Deposits remain `pending` before confirmation threshold is met
- Balance changes exactly once when transitioning to `confirmed`
- Transaction status and confirmation fields match documented behavior

## Documentation Updates

### PRD.md Updates
- **§4 (Goals):** Row 6 marked as ✅ Done with full confirmation policy details
- **§6 (User Flows):** Payment flow updated with new detection → verification → confirmation sequence
- **§8 (Status Ledger):** Confirmation-depth threshold moved from 🚧 Working to ✅ Done
- **§9.2 (Confirmation-depth policy):** Resolved with full policy documentation
- **§12 (Roadmap):** Confirmation-depth implementation marked ✅ Done

## Acceptance Criteria Met

✅ **The confirmation policy and rationale are documented.**
- Documented in PRD.md §9.2 with full rationale for 32-ledger threshold
- Policy clearly explains detection → verification → confirmation stages
- UX implications documented for merchants

✅ **Tests verify deposits remain pending before the policy is met.**
- `deposit_with_insufficient_confirmations_stays_verified` — Deposits with < 32 ledgers stay verified
- `balance_unchanged_when_payment_detected` — No balance changes before verified
- Balance stays in `pending` until `confirmed` status reached

✅ **Tests verify the balance changes exactly once when a deposit is confirmed.**
- `balance_moves_from_pending_to_available_on_confirmed` — Single transition from pending to available
- Multiple payments at different stages show correct balance state
- No duplicate or intermediate balance updates

✅ **Transaction status and confirmation fields match the documented behavior.**
- `confirmation_ledger` field populated on verification
- `confirmation_threshold` set to 32 for all new payments
- Status transitions: detected → verified → confirmed
- Ledger delta calculation correct in both queries and worker logic

## Future Considerations

1. **Configurable Threshold:** Current implementation fixes threshold at 32; could be made configurable per payment or merchant
2. **Ledger Event Stream:** Current poll-based approach could migrate to Horizon's streaming (SSE) API for lower latency
3. **Webhook Notification:** Ready-to-send payment.confirmed webhooks in finalize_payment phase
4. **Metrics:** Confirmation time distribution and pending balance trends
