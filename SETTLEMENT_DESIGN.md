# Settlement Pipeline Design & Operational Prerequisites

**Status:** Design Document (Task #1)  
**Last Updated:** 2026-10-08  
**Author:** M95 Platform Team

---

## Table of Contents

1. [Overview](#overview)
2. [Settlement Trigger & Timing](#settlement-trigger--timing)
3. [Operational Prerequisites](#operational-prerequisites)
4. [Complete Settlement Flow](#complete-settlement-flow)
5. [Safety Guarantees & Idempotency](#safety-guarantees--idempotency)
6. [Error Handling & Recovery](#error-handling--recovery)
7. [Operational Readiness Checklist](#operational-readiness-checklist)

---

## Overview

The settlement pipeline automates the flow of merchant deposits from individual Stellar wallets → platform settlement wallet → cNGN redemption to NGN → bank payouts.

**Key Flows:**
- **Sweep:** Move cNGN from individual merchant wallet to platform settlement wallet (on-chain)
- **Redemption:** Exchange cNGN for NGN with the approved issuer (off-chain API integration)
- **Payout:** Use NGN in the platform wallet to fund merchant bank transfers via Paystack

**No changes to payout mechanics:** Paystack remains the last-mile provider. The platform simply now funds payouts from redeemed NGN instead of waiting for merchant direct deposits.

---

## Settlement Trigger & Timing

### Trigger Type: **Scheduled + Balance-Threshold** (Hybrid)

The settlement pipeline is triggered by a **background worker** that runs on a configurable schedule. Each run performs a settlement cycle.

```
┌─────────────────────────────────────┐
│    Settlement Worker Cycle          │
│    (every N minutes/hours)          │
├─────────────────────────────────────┤
│ 1. Scan all eligible wallets        │
│ 2. Identify cNGN balance threshold  │
│ 3. Create sweep batches (idempotent)│
│ 4. Execute sweeps (on-chain)        │
│ 5. Execute redemptions (issuer API) │
│ 6. Emit audit logs & status events  │
└─────────────────────────────────────┘
```

**Configuration (new ENV vars):**
- `SETTLEMENT_ENABLED` – `true` or `false` (default: `false` until approvals)
- `SETTLEMENT_INTERVAL_SECS` – How often the worker runs (e.g., 3600 = 1 hour)
- `SETTLEMENT_WINDOW_START_UTC` – Optional: lock settlement to a specific UTC hour each day (e.g., `02:00` for 2 AM daily)
- `SETTLEMENT_MIN_BALANCE_STROOPS` – Minimum cNGN balance to trigger a sweep (e.g., 10 million stroops ≈ 10 cNGN)
- `SETTLEMENT_MAX_RETRIES` – Maximum attempts per sweep before marking terminal_failed (e.g., 5)
- `SETTLEMENT_ISSUER_REFERENCE` – The approved issuer's API endpoint and credentials (loaded from secure config/vault)

### Why Not Event-Driven?

**Scheduled approach is safer for:**
- **Compliance visibility:** Predictable settlement times align with banking & regulatory requirements
- **Crash recovery:** A restarted worker inherits idempotency keys and can resume mid-pipeline
- **Operational control:** Ops can pause settlement at deployment or during incidents without code changes
- **Batch efficiency:** Sweeping multiple merchants in one cycle reduces on-chain fees and API calls

### Idempotency Key: `sha256(wallet_id || window_start)`

Prevents duplicate sweeps if the worker crashes mid-run and restarts:

```rust
// Example
wallet_id = "GBAA...xyz"
window_start = "2026-10-08T02:00:00Z"
idempotency_key = sha256("GBAA...xyz2026-10-08T02:00:00Z")
```

If the worker restarts before updating the batch status, a new attempt will see the existing `idempotency_key` and skip (UNIQUE constraint on DB).

---

## Operational Prerequisites

**Before activation**, the following must be in place:

### 1. Issuer Integration Completed & Approved

- ✅ Redemption API endpoint documented (host, auth method, payload/response schema)
- ✅ Approved API credentials stored in secure config (e.g., AWS Secrets Manager)
- ✅ Sandbox testing completed (successful round-trip cNGN → NGN)
- ✅ Error scenarios mapped (network timeout, invalid amount, duplicate reference, etc.)
- ✅ Compliance & legal approval signed off on the redemption flow

**Configuration stored as:**
```env
SETTLEMENT_ISSUER_API_URL=https://issuer.example.com/redeem
SETTLEMENT_ISSUER_API_KEY=<aws-secrets-reference>  # never in .env
SETTLEMENT_ISSUER_MAX_TIMEOUT_SECS=30
```

### 2. Platform Settlement Wallet Created & Funded

- ✅ Stellar public key created (deterministic keypair for the platform)
- ✅ Private key encrypted at rest in secure config (AWS KMS, Vault, or equivalent)
- ✅ Wallet funded with XLM for transaction fees (1 XLM ≈ 10M stroops; ~3 million stroops per sweep tx)
- ✅ Wallet marked as `STELLAR_SYSTEM_WALLET_ADDRESS` in the environment

**Validation on startup:**
```rust
// src/main.rs checks:
let system_wallet = env::var("STELLAR_SYSTEM_WALLET_ADDRESS")?;
validate_stellar_address(&system_wallet)?; // must be a valid public key

// If SETTLEMENT_ENABLED=true, also verify system wallet is funded on Horizon
if settlement_enabled {
    let account = horizon.fetch_account(&system_wallet).await?;
    if account.native_balance < 1.0 {
        panic!("system wallet has insufficient XLM for sweep fees");
    }
}
```

### 3. Issuer Trust Line Established in Platform Wallet

- ✅ Platform settlement wallet must trust the cNGN issuer and have a non-zero limit
- ✅ Run one manual `change_trust` operation via `stellar-cli` or library before deployment

```bash
# One-time manual operation (before app startup):
stellar-cli set-options \
  --add-signer-weight 1 \
  --account PLATFORM_WALLET \
  --asset cNGN/ISSUER_ADDRESS
```

### 4. Deployment & Monitoring Infrastructure

- ✅ Background worker deployed as a reliable, auto-restarting component
- ✅ Audit log table monitored for `severity='error'` rows → ops alerting
- ✅ Failure rate dashboard: track `sweep_failed` and `terminal_failed` batches
- ✅ On-call runbook for manual recovery (e.g., force-retry a stuck batch)
- ✅ Log aggregation configured (CloudWatch, Datadog, etc.) to track settlement progress

### 5. Regulatory & Compliance Sign-Off

- ✅ Settlement flow reviewed by compliance & legal teams
- ✅ Approval documented (e.g., JIRA ticket, email consent, contract addendum)
- ✅ No real money settlement until all approvals are signed

---

## Complete Settlement Flow

### State Machine Diagram

```
IDENTIFIED (funds eligible)
    ↓
SWEEPING (Stellar tx submitted to Horizon)
    ├─ SUCCESS → SWEPT
    ├─ FAILURE (transient) → SWEEP_FAILED → [retry] → SWEEPING
    └─ FAILURE (terminal) → TERMINAL_FAILED [ops alert]
    ↓
SWEPT (on-chain confirmed)
    ↓
REDEEMING (issuer API call in progress)
    ├─ SUCCESS → REDEEMED
    ├─ FAILURE (transient) → REDEMPTION_FAILED → [retry] → REDEEMING
    └─ FAILURE (terminal) → TERMINAL_FAILED [ops alert]
    ↓
REDEEMED (issuer confirmed NGN credited)
    ↓
READY_FOR_PAYOUT (NGN in platform wallet; merchant can withdraw)
```

### Step-by-Step Execution

**Worker Loop (runs every SETTLEMENT_INTERVAL_SECS):**

```
1. Identify Eligible Wallets
   - Query all wallets with cNGN balance ≥ SETTLEMENT_MIN_BALANCE_STROOPS
   - Exclude wallets with in-flight sweeps (status ∉ {identified, sweeping})
   - Log: "eligibility_scan_started"

2. Create Sweep Batches (idempotent)
   FOR each eligible wallet:
     - Compute idempotency_key = sha256(wallet_id || window_start)
     - INSERT INTO sweep_batches (status='identified') IF NOT EXISTS
       (UNIQUE constraint ensures no duplicates on restart)
     - Log: "batch_identified" or "batch_skipped_duplicate"

3. Execute Sweeps
   FOR each batch with status='identified':
     - Load wallet's private key (decrypt from DB)
     - Query Horizon for current balance (sanity check: matches eligible_stroops)
     - Construct Stellar payment tx: cNGN wallet → platform wallet
     - Store tx_hash and xdr before submitting (crash recovery)
     - Transition: identified → sweeping
     - Submit to Horizon
     - Log: "sweep_started", "sweep_submitted"

4. Poll Horizon for Sweep Confirmation
   FOR each batch with status='sweeping':
     - Query Horizon for tx_hash status
     - On success:
       - Transition: sweeping → swept
       - Log: "sweep_confirmed"
     - On failure (transient):
       - Transition: sweeping → sweep_failed + increment retry_count
       - Log: "sweep_failed" (with error category, no secrets)
     - On max retries exceeded:
       - Transition: sweep_failed → terminal_failed
       - Log: "terminal_failure" (ops alert)

5. Execute Redemptions
   FOR each batch with status='swept':
     - Query issuer API to get the latest redemption exchange rate
     - Create NEW RedemptionAttempt row (status='pending', attempt_number++)
     - Construct redemption request payload (cNGN amount → NGN kobo)
     - Call issuer API with idempotency_key = sha256(batch_id || attempt_number)
     - Transition: swept → redeeming
     - Log: "redemption_started"

6. Poll Issuer for Redemption Confirmation
   FOR each batch with status='redeeming':
     - Query issuer API using sweep_batch_id to check attempt status
     - On success:
       - Update RedemptionAttempt: issuer_reference, ngn_amount_kobo, status='succeeded'
       - Transition: redeeming → redeemed
       - Log: "redemption_succeeded"
     - On failure (transient):
       - Update RedemptionAttempt: error_message, status='failed'
       - Transition: redeeming → redemption_failed + increment retry_count
       - Create NEW RedemptionAttempt for next retry attempt
       - Log: "redemption_failed" (with error category)
     - On max retries exceeded:
       - Transition: redemption_failed → terminal_failed
       - Log: "terminal_failure" (ops alert)

7. Mark Ready for Payout
   FOR each batch with status='redeemed':
     - Transition: redeemed → ready_for_payout
     - Log: "batch_completed"
     - NGN is now in the platform wallet; merchants can withdraw
```

### Atomic State Transitions

All transitions are **race-safe**:

```sql
UPDATE sweep_batches
SET status = $new_status, last_error = $error, retry_count = retry_count + $increment
WHERE id = $batch_id AND status = $expected_status;
```

Only the winning worker (first to execute this query) succeeds; others see `rows_affected = 0` and skip.

---

## Safety Guarantees & Idempotency

### Guarantee 1: No Duplicate Sweeps

- `UNIQUE(idempotency_key)` on `sweep_batches` table
- Restart of worker after batch creation will see the existing row and skip

### Guarantee 2: Funds Not Marked Settled Before Redemption Succeeds

- Batches transition to `ready_for_payout` **only after** `status='redeemed'`
- Merchants can only withdraw when `ready_for_payout = true`
- No early payout possible

### Guarantee 3: Reprocessing Same Funds Creates No Duplicates

- Sweep: idempotency_key prevents duplicate on-chain transactions
- Redemption: each attempt assigned a unique idempotency_key; issuer must detect & return duplicate

### Guarantee 4: Secrets Not Exposed in Logs

- `last_error` and `error_message` fields are **sanitised**:
  - Errors caught and mapped to fixed-text categories (e.g., "network_timeout", "invalid_amount")
  - Private keys, seed phrases, or raw error details never included
- Audit log detail column is JSON; application never inserts secret fields

### Guarantee 5: Crash Recovery

- Stellar tx hash and xdr stored **before** submission → on crash, worker can query Horizon
- Redemption attempt rows have idempotency key → issuer API can detect duplicate and return same outcome
- Window_start ensures batches belong to specific settlement cycles

---

## Error Handling & Recovery

### Transient Errors (Retryable)

**Sweep failures:**
- Network timeout
- Horizon temporarily unreachable
- Account sequence mismatch (rare but possible)

**Redemption failures:**
- Issuer API timeout
- Issuer temporarily unavailable
- Rate limit hit (5xx response)

**Action:** Increment `retry_count`, transition to `*_failed`, worker retries on next cycle.

### Terminal Errors (Require Ops Intervention)

- Max retries exceeded
- Issuer returns permanent rejection (e.g., "unsupported asset version")
- System wallet has insufficient XLM for fees

**Action:** Transition to `terminal_failed`, emit audit log entry with `severity='error'`, ops alert triggered.

### Recovery Procedures

**Manual retry of terminal_failed batch:**

```sql
-- Ops can manually reset a stuck batch (requires explicit CLI flag or API key)
UPDATE sweep_batches
SET status = 'identified', retry_count = 0, last_error = NULL
WHERE id = $batch_id AND status = 'terminal_failed';
```

**On-call runbook:**

1. Query audit log for recent errors: `SELECT * FROM settlement_audit_log WHERE severity = 'error' ORDER BY occurred_at DESC LIMIT 10`
2. Identify root cause (network, issuer API, system wallet funding)
3. If transient (e.g., issuer temporarily down):
   - Wait 15 minutes, worker will retry automatically
   - Monitor: `SELECT COUNT(*) FROM sweep_batches WHERE status IN ('sweep_failed', 'redemption_failed')`
4. If persistent (e.g., system wallet unfunded):
   - Fund system wallet, then manually reset batch status
   - Resubmit via worker next cycle

---

## Operational Readiness Checklist

**Before `SETTLEMENT_ENABLED=true`:**

- [ ] Issuer integration complete & tested in sandbox
- [ ] Issuer API credentials stored in AWS Secrets Manager (or equivalent)
- [ ] Platform settlement wallet created & its address registered as `STELLAR_SYSTEM_WALLET_ADDRESS`
- [ ] Platform wallet funded with minimum 1 XLM (covers ~300+ sweep transactions)
- [ ] Trust line (`change_trust`) established between platform wallet and cNGN issuer
- [ ] Background worker deployment configured (auto-restart, resource limits)
- [ ] Audit log monitoring & alerting set up (error threshold, ops Slack channel)
- [ ] Runbook drafted and on-call team trained
- [ ] Compliance & legal sign-off obtained (dated approval in ticket/email)
- [ ] Staging environment settlement cycle completed successfully (end-to-end test)
- [ ] Production deployment rolled out with `SETTLEMENT_ENABLED=false` initially
- [ ] Final sign-off from product & compliance; `SETTLEMENT_ENABLED=true` in production

---

## Environment Variables Summary

| Variable | Required | Default | Description |
|---|---|---|---|
| `SETTLEMENT_ENABLED` | No | `false` | Enable settlement pipeline globally |
| `SETTLEMENT_INTERVAL_SECS` | No | `3600` | Worker cycle interval (seconds) |
| `SETTLEMENT_WINDOW_START_UTC` | No | — | Optional UTC hour lock (e.g., `02:00` for daily 2 AM) |
| `SETTLEMENT_MIN_BALANCE_STROOPS` | No | `10000000` | Minimum cNGN balance to sweep (10M = ~10 cNGN) |
| `SETTLEMENT_MAX_RETRIES` | No | `5` | Max attempts per batch before terminal_failed |
| `SETTLEMENT_ISSUER_API_URL` | Yes (if enabled) | — | Issuer redemption endpoint |
| `SETTLEMENT_ISSUER_API_KEY` | Yes (if enabled) | — | Issuer API credentials (from Secrets Manager) |
| `SETTLEMENT_ISSUER_MAX_TIMEOUT_SECS` | No | `30` | Issuer API call timeout |

---

## Summary: Operational Requirements

1. **Trigger Type:** Scheduled background worker (interval + optional daily lock)
2. **Idempotency:** Window-based + wallet-specific (no duplicate sweeps on restart)
3. **Safety:** Funds not marked settled until redemption succeeds; all transitions are atomic
4. **Prerequisites:**
   - Issuer API integration tested & approved
   - Platform settlement wallet funded
   - Trust line established
   - Background worker deployed with monitoring
   - Compliance sign-off obtained

5. **Deployment:** `SETTLEMENT_ENABLED=false` by default; flip to `true` after final approvals

The pipeline is now ready to be implemented with confidence in operational safety and recovery.
