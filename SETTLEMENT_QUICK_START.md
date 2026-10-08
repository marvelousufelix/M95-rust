# Settlement Pipeline: Quick Start Guide for Developers

**Status:** Foundation Complete (Core Services Ready)  
**Last Updated:** October 8, 2026

---

## One-Minute Overview

The settlement pipeline moves funds from merchant wallets → platform wallet → bank payouts:

```
Merchant Wallet (cNGN)
         ↓
    [SWEEP] ← Stellar transaction (on-chain)
         ↓
Platform Wallet (cNGN)
         ↓
  [REDEEM] ← Issuer API call
         ↓
Platform Wallet (NGN)
         ↓
  [PAYOUT] ← Paystack transfer (existing)
```

**Three services handle this:**
- **SweepBatchService** — manage on-chain sweeps (status tracking, tx storage, idempotency)
- **RedemptionAttemptService** — track issuer API calls (retry logic, result storage)
- **AuditLogService** — record all events (no secrets exposed)

**All operations are idempotent and crash-safe.**

---

## Key Files

| File | Purpose |
|---|---|
| `src/services/settlement.rs` | Service layer (the business logic) |
| `src/blockchain/issuer.rs` | Issuer integration trait + data structures |
| `src/config.rs` | Settlement configuration (SettlementConfig, IssuerConfig) |
| `SETTLEMENT_DESIGN.md` | Full design document + prerequisites |
| `SETTLEMENT_IMPLEMENTATION_STATUS.md` | Progress & acceptance criteria |

---

## Using the Services

### SweepBatchService

**Create a batch (idempotent):**
```rust
use crate::services::settlement::SweepBatchService;
use uuid::Uuid;
use chrono::Utc;

let (batch_id, is_new) = SweepBatchService::create_batch(
    &pool,
    wallet_id,
    merchant_id,
    window_start,  // DateTime<Utc>
    eligible_stroops,
    "cNGN",
).await?;

if is_new {
    println!("New batch created: {}", batch_id);
} else {
    println!("Batch already exists: {}", batch_id);
}
```

**Get all batches in a state:**
```rust
use crate::models::settlement::SweepStatus;

let pending_sweeps = SweepBatchService::batches_in_status(
    &pool,
    SweepStatus::Identified,  // or Sweeping, Swept, etc.
).await?;

for batch in pending_sweeps {
    println!("Batch {} ready to sweep", batch.id);
}
```

**Transition state (atomic, race-safe):**
```rust
use crate::models::settlement::TransitionSweepStatus;

let transition = TransitionSweepStatus {
    id: batch_id,
    expected_status: SweepStatus::Identified,
    new_status: SweepStatus::Sweeping,
    last_error: None,
    increment_retry: false,
};

let rows_affected = SweepBatchService::transition_status(&pool, &transition).await?;

if rows_affected == 1 {
    println!("Transitioned successfully");
} else {
    println!("Race condition: another worker already transitioned this batch");
}
```

**Store Stellar tx before submission (crash recovery):**
```rust
SweepBatchService::set_sweep_tx_details(
    &pool,
    batch_id,
    "0a1b2c3d...",  // tx hash
    "AAAAAgAA...",  // signed XDR envelope
).await?;

// Now safe to submit to Horizon; if we crash, worker can check Horizon
// using the stored hash to determine outcome
```

### RedemptionAttemptService

**Create a new redemption attempt (auto-increments):**
```rust
use crate::services::settlement::RedemptionAttemptService;

let attempt = RedemptionAttemptService::create_attempt(
    &pool,
    batch_id,
    amount_stroops,  // Amount to redeem
).await?;

println!("Created attempt #{}: {}", attempt.attempt_number, attempt.id);
// First attempt is #1, second is #2, etc.
// idempotency_key automatically computed: sha256(batch_id || attempt_number)
```

**Mark attempt as submitted:**
```rust
RedemptionAttemptService::mark_submitted(&pool, attempt_id).await?;
```

**Mark as succeeded (issuer confirmed NGN):**
```rust
use crate::models::settlement::RedemptionSuccess;

let success = RedemptionSuccess {
    attempt_id,
    issuer_reference: "ISS-12345".to_string(),  // Issuer's tx ID
    ngn_amount_kobo: 100_000,                   // 1,000 NGN = 100,000 kobo
};

RedemptionAttemptService::mark_succeeded(&pool, &success).await?;
```

**Mark as failed (will retry next cycle):**
```rust
use crate::models::settlement::RedemptionFailure;

let failure = RedemptionFailure {
    attempt_id,
    error_message: "network_timeout".to_string(),  // Sanitised!
    issuer_reference: None,
};

RedemptionAttemptService::mark_failed(&pool, &failure).await?;
```

**Get all in-flight attempts (for recovery):**
```rust
let in_flight = RedemptionAttemptService::in_flight_attempts(&pool).await?;
for attempt in in_flight {
    println!("In-flight: batch {} attempt {}", attempt.sweep_batch_id, attempt.attempt_number);
}
```

### AuditLogService

**Log an event (append-only):**
```rust
use crate::services::settlement::AuditLogService;
use crate::models::settlement::{NewAuditLogEntry, AuditSeverity, event};

let entry = NewAuditLogEntry {
    sweep_batch_id: Some(batch_id),
    redemption_attempt_id: None,
    merchant_id: Some(merchant_id),
    event_name: event::BATCH_IDENTIFIED.to_string(),
    detail: serde_json::json!({
        "wallet_id": wallet_id.to_string(),
        "amount_stroops": 5_000_000i64,
        "asset": "cNGN"
    }),
    severity: AuditSeverity::Info,
};

let entry_id = AuditLogService::log_event(&pool, &entry).await?;
println!("Logged event: {}", entry_id);
```

**Query batch events (ops dashboard):**
```rust
let events = AuditLogService::events_for_batch(&pool, batch_id, 50).await?;
for event in events {
    println!("{}: {} ({})", event.occurred_at, event.event_name, event.severity);
}
```

**Get recent errors (ops alerting):**
```rust
let errors = AuditLogService::recent_errors(&pool, 20).await?;
for error in errors {
    println!("ERROR at {}: {}", error.occurred_at, error.detail);
}
```

---

## Configuration

### Environment Variables

```bash
# Enable settlement (default: false; requires approvals)
SETTLEMENT_ENABLED=true

# Worker frequency (default: 3600 seconds = 1 hour)
SETTLEMENT_INTERVAL_SECS=3600

# Optional: lock to specific UTC hour (e.g., 02:00 daily)
SETTLEMENT_WINDOW_START_UTC=02:00

# Minimum balance to sweep (default: 10M stroops ≈ 10 cNGN)
SETTLEMENT_MIN_BALANCE_STROOPS=10000000

# Retry limit before ops alert (default: 5)
SETTLEMENT_MAX_RETRIES=5

# Issuer API
SETTLEMENT_ISSUER_API_URL=https://issuer.example.com/redeem
SETTLEMENT_ISSUER_API_KEY=<secret>
SETTLEMENT_ISSUER_MAX_TIMEOUT_SECS=30
```

### Load config in your code:

```rust
use crate::config::AppConfig;

let config = AppConfig::from_env()?;

if config.settlement.enabled {
    println!("Settlement enabled, running every {} seconds", config.settlement.interval_secs);
    println!("Issuer API: {}", config.settlement.issuer.api_url);
    // Note: config.settlement.issuer.api_key will print "[REDACTED]" for safety
}
```

---

## Common Patterns

### Idempotency: Creating a Sweep (Won't Duplicate on Restart)

```rust
// Worker task:
let window_start = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();

// Even if worker restarts, same wallet + window = same idempotency_key
let (batch_id, is_new) = SweepBatchService::create_batch(
    &pool,
    wallet_id,
    merchant_id,
    window_start,
    eligible_stroops,
    "cNGN",
).await?;

// UNIQUE constraint ensures no duplicates; is_new tells us if fresh or existing
```

### Error Sanitisation (Never Expose Secrets)

```rust
use crate::services::settlement::sanitise_error_message;

// Raw error from Stellar API (might contain wallet info)
let raw_error = "Sequence error for account GBA...xyz: expected seq 123";

// Sanitise it
let safe_error = sanitise_error_message(&raw_error);
// Result: "invalid_request" or similar (never raw text)

// Log it safely
let entry = NewAuditLogEntry {
    // ...
    error_message: Some(safe_error),
    // ...
};
```

### Crash Recovery (Storing Tx Before Submission)

```rust
// Step 1: Construct and sign Stellar tx
let tx_hash = "a1b2c3d4e5f6...";
let tx_xdr = "AAAAAgAA...";

// Step 2: ATOMICALLY store in DB
SweepBatchService::set_sweep_tx_details(&pool, batch_id, tx_hash, tx_xdr).await?;

// Step 3: NOW submit to Horizon
let response = submit_to_horizon(tx_xdr).await?;

// If crash here: on restart, worker can query:
// SELECT stellar_tx_hash, stellar_tx_xdr FROM sweep_batches WHERE id = $batch_id
// Then check Horizon using stored hash to see if it confirmed
```

### Race-Safe State Transitions

```rust
// Multiple workers may process same batch; only one should win

let transition = TransitionSweepStatus {
    id: batch_id,
    expected_status: SweepStatus::Identified,
    new_status: SweepStatus::Sweeping,
    last_error: None,
    increment_retry: false,
};

let rows_affected = SweepBatchService::transition_status(&pool, &transition).await?;

if rows_affected == 1 {
    // Success! Only this worker transitioned the batch.
    process_sweep(&batch).await?;
} else {
    // Another worker already transitioned it.
    // This worker skips to the next batch.
}
```

---

## Testing

### Unit Tests

All services have unit tests. Run with:
```bash
cargo test settlement --lib
```

Tests cover:
- ✅ Idempotency key generation (deterministic, differs by input)
- ✅ Error sanitisation (no secrets leak)
- ✅ Status enum round-trips (parse/serialize)

### Integration Tests (Coming Soon)

```bash
cargo test settlement --test '*'
```

Will test:
- Happy path: sweep → redemption → ready_for_payout
- Retryable failures: network timeout, rate limit
- Terminal failures: invalid amount, account closed
- Idempotency: restart doesn't duplicate
- Crash recovery: worker determines outcome from stored tx hash

---

## Issuer Integration

### Mock Implementation (for Testing)

```rust
use crate::blockchain::issuer::{IssuerClient, RedemptionRequest, RedemptionOutcome, RedemptionSuccess};
use async_trait::async_trait;

pub struct MockIssuerClient;

#[async_trait]
impl IssuerClient for MockIssuerClient {
    async fn redeem(&self, request: RedemptionRequest) -> RedemptionOutcome {
        // Immediately succeed for testing
        RedemptionOutcome::Success(RedemptionSuccess {
            reference: "MOCK-12345".to_string(),
            ngn_amount_kobo: (request.amount_stroops / 1_000_000 * 1_000) as i64,
            memo: Some("Mock redemption".to_string()),
        })
    }

    async fn query_redemption_status(&self, _key: &str) 
        -> Result<Option<RedemptionOutcome>, String> {
        Ok(None)
    }
}
```

### Real Implementation (Coming Soon)

```rust
pub struct HttpIssuerClient {
    api_url: String,
    api_key: String,
    timeout: Duration,
}

#[async_trait]
impl IssuerClient for HttpIssuerClient {
    async fn redeem(&self, request: RedemptionRequest) -> RedemptionOutcome {
        // TODO: POST request to issuer API
        // Parse response into RedemptionOutcome::Success or ::TerminalFailure
    }

    async fn query_redemption_status(&self, key: &str) 
        -> Result<Option<RedemptionOutcome>, String> {
        // TODO: GET request to check status
    }
}
```

---

## State Machine Diagram

```
                    IDENTIFIED
                        ↓
                    (sweep_tx created)
                        ↓
                    SWEEPING
                    /      \
                  SUCCESS   FAILURE (transient)
                  /              \
                SWEPT        SWEEP_FAILED ← (retry allowed)
                 ↓                  ↓
            (redeem_tx created)  (retry loop)
                 ↓
             REDEEMING
             /      \
           SUCCESS   FAILURE (transient)
           /              \
        REDEEMED      REDEMPTION_FAILED ← (retry allowed)
           ↓                  ↓
       (funds       (retry loop)
        received)
           ↓
    READY_FOR_PAYOUT ← Only here are funds safe for payout!
```

**Key:** `READY_FOR_PAYOUT` is ONLY reached after `REDEEMED` confirmed. Funds never marked settled prematurely.

---

## Next Steps

1. **Implement worker** (`src/blockchain/settlement_worker.rs`)
   - Event loop: run every N seconds
   - Eligible wallet scan
   - Batch creation
   - Sweep execution
   - Redemption execution

2. **Implement tests** (`tests/settlement_flow.rs`)
   - Happy path
   - Retryable failures
   - Terminal failures
   - Idempotency

3. **Implement issuer HTTP client** (mock → real)
   - POST /redeem endpoint
   - GET /status endpoint
   - Error handling

4. **Deploy & monitor**
   - Background worker running reliably
   - Audit logs monitored
   - On-call runbook ready

---

## Questions?

- **Design questions:** See `SETTLEMENT_DESIGN.md`
- **Status & acceptance criteria:** See `SETTLEMENT_IMPLEMENTATION_STATUS.md`
- **Full summary:** See `SETTLEMENT_FOUNDATION_SUMMARY.md`
- **Code:** See `src/services/settlement.rs`, `src/blockchain/issuer.rs`

---

**Ready to build the worker? Let's go! 🚀**
