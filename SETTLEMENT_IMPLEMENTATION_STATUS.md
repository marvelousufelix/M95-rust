# Settlement Pipeline Implementation Status

**Date:** 2026-10-08  
**Status:** Tasks 1-3 Complete (Core Infrastructure Ready)

---

## Completed Work

### Task #1: Settlement Design & Operational Prerequisites ✅

**Deliverable:** `SETTLEMENT_DESIGN.md`

Comprehensive documentation covering:
- **Trigger Type:** Scheduled background worker (interval-based + optional daily UTC lock)
- **Idempotency:** `sha256(wallet_id || window_start)` prevents duplicate sweeps on restart
- **State Machine:** 9 states (identified → sweeping → swept → redeeming → redeemed → ready_for_payout)
- **Safety Guarantees:** Funds not settled until redemption succeeds; all transitions atomic with race-safe WHERE conditions
- **Prerequisites Checklist:** Issuer integration, system wallet setup, trust line establishment, monitoring infrastructure
- **Error Recovery:** Transient vs. terminal errors, ops runbook, manual retry procedures

**Key Decision:** Scheduled worker is safer than event-driven for compliance visibility and crash recovery.

---

### Task #2: Settlement Service Layer ✅

**Deliverable:** `src/services/settlement.rs` (590 lines)

**Components:**
1. **SweepBatchService**
   - `create_batch()` — idempotent via UNIQUE idempotency_key constraint
   - `transition_status()` — atomic race-safe state transitions with WHERE id AND expected_status
   - `set_sweep_tx_details()` — store tx_hash and xdr before Horizon submission (crash recovery)
   - `batches_in_status()` — fetch pending batches for worker processing

2. **RedemptionAttemptService**
   - `create_attempt()` — auto-increment attempt_number, compute idempotency_key
   - `mark_submitted/succeeded/failed()` — state updates with issuer details
   - `in_flight_attempts()` — recovery queries for pending attempts
   - `latest_attempt()` — fetch most recent attempt for a batch

3. **AuditLogService**
   - `log_event()` — append-only audit trail (never modified)
   - `events_for_batch/merchant/recent_errors()` — ops queries for monitoring

4. **Error Sanitisation**
   - `sanitise_error_message()` — maps raw errors to fixed-text categories (e.g., "network_timeout", "invalid_request")
   - Ensures no secrets leak into audit logs

**All Services:**
- Database-focused with proper error propagation
- Unit tests for idempotency key generation and determinism
- Proper transaction handling via sqlx queries

---

### Task #3: Issuer Integration Interface ✅

**Deliverable:** `src/blockchain/issuer.rs` (278 lines)

**Data Structures:**
- **RedemptionRequest** — payload sent to issuer (idempotency_key, amount_stroops, merchant_reference, memo)
- **RedemptionSuccess** — issuer response on success (reference, ngn_amount_kobo, memo)
- **RedemptionError** — error response (code, message, retryable flag, reference)
- **RedemptionOutcome** enum — terminal outcome (Success, RetryableFailure, TerminalFailure, NetworkError)

**Trait:**
- `IssuerClient` — async trait for redemption operations
  - `async fn redeem()` — submit redemption request
  - `async fn query_redemption_status()` — poll for completion

**Helper Methods:**
- `RedemptionError::infer_retryable()` — heuristic to classify terminal vs. transient errors
- `RedemptionOutcome::category()`, `is_final()`, `reference()` — observation helpers

**Tests:**
- 11 unit tests covering error inference heuristics, outcome categorization, and edge cases

---

## Updated Configuration

### Config Structs Added

**`SettlementConfig`:**
```rust
pub struct SettlementConfig {
    pub enabled: bool,                      // global enable/disable
    pub interval_secs: u64,                 // worker cycle frequency
    pub window_start_utc: Option<String>,   // optional daily lock (e.g., "02:00")
    pub min_balance_stroops: i64,           // sweep threshold
    pub max_retries: i32,                   // retry limit before terminal_failed
    pub issuer: IssuerConfig,               // issuer API credentials
}

pub struct IssuerConfig {
    pub api_url: String,                    // redemption endpoint
    pub api_key: SecretString,              // API credentials (redacted in logs)
    pub max_timeout_secs: u64,              // API call timeout
}
```

### Environment Variables Added

| Variable | Default | Description |
|---|---|---|
| `SETTLEMENT_ENABLED` | `false` | Global enable (requires approvals) |
| `SETTLEMENT_INTERVAL_SECS` | `3600` | Worker cycle frequency (seconds) |
| `SETTLEMENT_WINDOW_START_UTC` | — | Optional daily lock (e.g., "02:00") |
| `SETTLEMENT_MIN_BALANCE_STROOPS` | `10000000` | Sweep threshold (~10 cNGN) |
| `SETTLEMENT_MAX_RETRIES` | `5` | Retry limit before terminal_failed |
| `SETTLEMENT_ISSUER_API_URL` | — | Issuer redemption endpoint |
| `SETTLEMENT_ISSUER_API_KEY` | — | Issuer API credentials |
| `SETTLEMENT_ISSUER_MAX_TIMEOUT_SECS` | `30` | API call timeout |

**Note:** `.env.example` has been updated with comprehensive documentation.

---

## Files Created/Modified

- ✅ `SETTLEMENT_DESIGN.md` — new, 394 lines
- ✅ `src/services/settlement.rs` — new, 590 lines
- ✅ `src/blockchain/issuer.rs` — new, 278 lines
- ✅ `src/config.rs` — updated (added SettlementConfig, IssuerConfig, env loading)
- ✅ `src/services/mod.rs` — updated (export settlement)
- ✅ `src/blockchain/mod.rs` — updated (export issuer)
- ✅ `.env.example` — updated (settlement variables with docs)

---

## Remaining Work (Tasks 4-9)

### Task #4: Worker Orchestration & Retry Logic

Implement the settlement worker that:
- Runs on schedule (SETTLEMENT_INTERVAL_SECS)
- Identifies eligible wallets
- Creates sweep batches
- Constructs and submits Stellar sweep transactions
- Polls Horizon for confirmation
- Calls issuer API for redemption
- Polls issuer for completion
- Marks batches ready for payout or terminal_failed

**Estimated effort:** 300-400 lines of orchestration logic

### Task #5: Comprehensive Integration Tests

Test paths:
1. **Happy path:** Sweep succeeds → Redemption succeeds → Ready for payout
2. **Retryable failures:** Network timeout on sweep, issuer temporarily unavailable
3. **Terminal failures:** Insufficient XLM, invalid merchant reference, redemption rejected
4. **Idempotency:** Worker restart doesn't create duplicate sweeps

**Estimated effort:** 400-500 lines of test code

### Task #6: Operational Documentation

Update README, API.md, and create runbooks for:
- Settling up before going live
- Monitoring the settlement worker
- Handling stuck batches
- Emergency stop procedures

---

## Acceptance Criteria Status

| Criterion | Status | Evidence |
|---|---|---|
| Settlement trigger & prerequisites documented | ✅ | SETTLEMENT_DESIGN.md |
| Tests cover success, retryable, terminal paths | 🔄 | To be implemented (Task #5) |
| Funds not marked settled until redemption succeeds | ✅ | SweepStatus::ReadyForPayout only after Redeemed |
| Reprocessing same funds creates no duplicates | ✅ | UNIQUE idempotency_key + audit trail |
| Logs identify progress without exposing secrets | ✅ | AuditLogService + sanitise_error_message() |

---

## Next Steps

1. **Task #4:** Implement settlement worker (`src/blockchain/settlement_worker.rs`)
   - Sweep batch creation loop
   - Stellar transaction orchestration
   - Issuer API integration (mock HTTP client)
   - Retry logic with exponential backoff

2. **Task #5:** Integration tests (`tests/settlement_flow.rs`)
   - Test database setup fixtures
   - Mock issuer client
   - Test all state transitions

3. **Task #6-9:** Documentation, configuration, and acceptance review

---

## Architecture Notes

### Secrets Safety

- Wallet private keys encrypted at rest with AES-256-GCM
- Issuer API key wrapped in `SecretString` (redacted in logs/debug output)
- Error messages sanitised before logging (no raw API responses)
- Audit logs store only JSON with non-sensitive fields

### Idempotency & Crash Recovery

- **Sweep idempotency:** `UNIQUE(idempotency_key)` on sweep_batches
- **Redemption idempotency:** Issuer detects duplicate via idempotency_key
- **Tx hash stored before submission:** On crash, worker can query Horizon to determine outcome

### Database Atomicity

- All state transitions use `WHERE id = $id AND status = $expected_status`
- Only one worker wins; others see `rows_affected = 0` and skip

---

## Deployment Readiness

**Before `SETTLEMENT_ENABLED=true`:**
- [ ] Issuer integration tested in sandbox ← Task #5 (mock)
- [ ] Background worker deployed with monitoring ← Task #4
- [ ] Staging environment settlement cycle succeeds ← Task #5
- [ ] Compliance & legal approvals obtained ← Task #6
- [ ] Production deployment rolled out with flag=false
- [ ] Final sign-off; flag flipped to true

---

## Questions & Open Items

1. **Stellar Network:** Are we using testnet or mainnet for initial deployment? (affects fees, stability)
2. **Issuer Sandbox:** What is the exact redemption API schema/auth method?
3. **NGN Precision:** Is the issuer API returning NGN in kobo (minor units) or naira?
4. **Manual Retry:** Should ops have a CLI to retry stuck batches, or only via API?

**These will be clarified during implementation of Tasks #4-5.**

---

## Summary

The settlement pipeline foundation is now in place:
- **Core services** ready to handle sweep, redemption, and audit operations
- **Configuration** flexible for different issuer integrations
- **Safety guarantees** enforced at the database layer (atomicity, idempotency, secrets)
- **Clear design** documented for operational deployment

Next phase: Worker orchestration, end-to-end testing, and operational runbooks.
