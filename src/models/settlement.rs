/// Database row types and DTOs for the settlement pipeline.
///
/// These types map directly to the three settlement tables:
///   - `sweep_batches`        → [`SweepBatch`]
///   - `redemption_attempts`  → [`RedemptionAttempt`]
///   - `settlement_audit_log` → [`AuditLogEntry`]
///
/// # Secrets safety
/// None of these types hold decrypted private keys or seeds. The `last_error`
/// and `error_message` fields are sanitised at the service layer before
/// insertion — error variants use fixed-text messages, never raw key material.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// SweepBatch
// ---------------------------------------------------------------------------

/// Represents one row in `sweep_batches`.
///
/// A sweep batch is created once per wallet per settlement window and tracks
/// the full lifecycle from fund identification through on-chain sweep,
/// cNGN redemption, and final payout readiness.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SweepBatch {
    pub id: Uuid,

    /// The wallet whose balance is being swept.
    pub wallet_id: Uuid,

    /// Denormalised merchant reference for efficient per-merchant queries.
    pub merchant_id: Uuid,

    /// The start of the settlement window this batch belongs to.
    /// Used (together with `wallet_id`) to compute `idempotency_key`.
    pub window_start: DateTime<Utc>,

    /// `sha256(wallet_id::text || window_start.to_rfc3339())`.
    /// `UNIQUE` constraint in the DB prevents duplicate batches.
    pub idempotency_key: String,

    /// How many stroops were eligible at batch creation time.
    pub eligible_stroops: i64,

    /// Asset being swept (typically `"cNGN"`).
    pub asset: String,

    /// Current state in the settlement state machine.
    /// One of the values defined in [`SweepStatus`].
    pub status: String,

    /// Stellar transaction hash, stored *before* Horizon submission so a
    /// crash-recovery scan can check Horizon for the outcome.
    /// `NULL` until the Stellar tx is constructed.
    pub stellar_tx_hash: Option<String>,

    /// The signed Stellar XDR envelope. Stored so the worker can resubmit on
    /// a transient network failure without re-signing.
    /// `NULL` until the tx is constructed.
    pub stellar_tx_xdr: Option<String>,

    /// Number of times a sweep or redemption has been retried.
    pub retry_count: i32,

    /// The last error that caused a state transition to `sweep_failed` or
    /// `redemption_failed`. Sanitised — contains no secrets or key material.
    pub last_error: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The valid states for [`SweepBatch::status`].
///
/// Stored as `TEXT` in the database; the `CHECK` constraint there mirrors this enum.
/// Use [`SweepStatus::as_str`] when writing to the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepStatus {
    /// Eligible funds identified; sweep not yet started.
    Identified,
    /// Stellar transaction constructed and submitted; awaiting confirmation.
    Sweeping,
    /// Stellar payment confirmed on-chain.
    Swept,
    /// Issuer redemption API call in progress.
    Redeeming,
    /// Issuer confirmed NGN credited.
    Redeemed,
    /// NGN available; merchant payout may proceed.
    ReadyForPayout,
    /// On-chain sweep failed (transient); eligible for retry.
    SweepFailed,
    /// Issuer redemption failed (transient); eligible for retry.
    RedemptionFailed,
    /// Exceeded maximum retries; requires ops intervention.
    TerminalFailed,
}

impl SweepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SweepStatus::Identified => "identified",
            SweepStatus::Sweeping => "sweeping",
            SweepStatus::Swept => "swept",
            SweepStatus::Redeeming => "redeeming",
            SweepStatus::Redeemed => "redeemed",
            SweepStatus::ReadyForPayout => "ready_for_payout",
            SweepStatus::SweepFailed => "sweep_failed",
            SweepStatus::RedemptionFailed => "redemption_failed",
            SweepStatus::TerminalFailed => "terminal_failed",
        }
    }
}

impl std::fmt::Display for SweepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for SweepStatus {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "identified" => Ok(SweepStatus::Identified),
            "sweeping" => Ok(SweepStatus::Sweeping),
            "swept" => Ok(SweepStatus::Swept),
            "redeeming" => Ok(SweepStatus::Redeeming),
            "redeemed" => Ok(SweepStatus::Redeemed),
            "ready_for_payout" => Ok(SweepStatus::ReadyForPayout),
            "sweep_failed" => Ok(SweepStatus::SweepFailed),
            "redemption_failed" => Ok(SweepStatus::RedemptionFailed),
            "terminal_failed" => Ok(SweepStatus::TerminalFailed),
            other => Err(format!("unknown SweepStatus: {other}")),
        }
    }
}

/// Input for inserting a new sweep batch row.
#[derive(Debug, Clone)]
pub struct NewSweepBatch {
    pub wallet_id: Uuid,
    pub merchant_id: Uuid,
    pub window_start: DateTime<Utc>,
    pub idempotency_key: String,
    pub eligible_stroops: i64,
    pub asset: String,
}

/// Input for the atomic state transition update.
///
/// The UPDATE uses `WHERE id = $id AND status = $expected_status` to make
/// the transition race-safe — only one worker can win.
#[derive(Debug, Clone)]
pub struct TransitionSweepStatus {
    pub id: Uuid,
    pub expected_status: SweepStatus,
    pub new_status: SweepStatus,
    /// Optional error description (sanitised). Set when transitioning to a
    /// `*_failed` state. Cleared when retrying.
    pub last_error: Option<String>,
    /// Whether to increment the retry counter in this transition.
    pub increment_retry: bool,
}

/// Update carrying the Stellar tx hash and XDR, stored atomically before
/// Horizon submission to enable crash recovery.
#[derive(Debug, Clone)]
pub struct SetSweepTxDetails {
    pub id: Uuid,
    pub stellar_tx_hash: String,
    pub stellar_tx_xdr: String,
}

// ---------------------------------------------------------------------------
// RedemptionAttempt
// ---------------------------------------------------------------------------

/// Represents one row in `redemption_attempts`.
///
/// A new row is inserted for each attempt to redeem cNGN with the issuer API.
/// Multiple rows may exist for the same `sweep_batch_id` when earlier attempts
/// failed and were retried.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RedemptionAttempt {
    pub id: Uuid,

    /// The parent sweep batch.
    pub sweep_batch_id: Uuid,

    /// 1-based counter; monotonically increasing per `sweep_batch_id`.
    pub attempt_number: i32,

    /// `sha256(sweep_batch_id::text || attempt_number::text)`.
    /// Sent to the issuer as the redemption reference for deduplication.
    pub idempotency_key: String,

    /// The amount submitted to the issuer (in cNGN stroops).
    pub amount_stroops: i64,

    /// Current state of this attempt.
    pub status: String,

    /// Issuer-assigned reference, populated on success or if the issuer returns
    /// a reference alongside a failure response.
    pub issuer_reference: Option<String>,

    /// The NGN amount (in kobo) that the issuer credited. `NULL` until success.
    pub ngn_amount_kobo: Option<i64>,

    /// Sanitised error detail for failed attempts. No secrets.
    pub error_message: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The valid states for [`RedemptionAttempt::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedemptionStatus {
    /// About to be submitted to the issuer.
    Pending,
    /// API call sent; awaiting callback or poll result.
    Submitted,
    /// Issuer confirmed NGN credited.
    Succeeded,
    /// This attempt failed; a new attempt may be created if retries remain.
    Failed,
}

impl RedemptionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RedemptionStatus::Pending => "pending",
            RedemptionStatus::Submitted => "submitted",
            RedemptionStatus::Succeeded => "succeeded",
            RedemptionStatus::Failed => "failed",
        }
    }
}

impl std::fmt::Display for RedemptionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for RedemptionStatus {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "pending" => Ok(RedemptionStatus::Pending),
            "submitted" => Ok(RedemptionStatus::Submitted),
            "succeeded" => Ok(RedemptionStatus::Succeeded),
            "failed" => Ok(RedemptionStatus::Failed),
            other => Err(format!("unknown RedemptionStatus: {other}")),
        }
    }
}

/// Input for inserting a new redemption attempt.
#[derive(Debug, Clone)]
pub struct NewRedemptionAttempt {
    pub sweep_batch_id: Uuid,
    pub attempt_number: i32,
    pub idempotency_key: String,
    pub amount_stroops: i64,
}

/// Outcome of a successful redemption, used to update the attempt row.
#[derive(Debug, Clone)]
pub struct RedemptionSuccess {
    pub attempt_id: Uuid,
    pub issuer_reference: String,
    pub ngn_amount_kobo: i64,
}

/// Outcome of a failed redemption attempt.
#[derive(Debug, Clone)]
pub struct RedemptionFailure {
    pub attempt_id: Uuid,
    /// Sanitised error message, no secrets.
    pub error_message: String,
    /// Issuer reference if the issuer returned one even on failure.
    pub issuer_reference: Option<String>,
}

// ---------------------------------------------------------------------------
// AuditLogEntry
// ---------------------------------------------------------------------------

/// Represents one row in `settlement_audit_log`.
///
/// This table is append-only. The application layer MUST ensure that the
/// `detail` field contains no secrets before constructing this struct.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuditLogEntry {
    pub id: Uuid,

    /// The sweep batch this event relates to. `NULL` for system-level events
    /// that occur before a batch is created (e.g. eligibility scan start).
    pub sweep_batch_id: Option<Uuid>,

    /// The redemption attempt this event relates to, if applicable.
    pub redemption_attempt_id: Option<Uuid>,

    /// Denormalised merchant reference for efficient per-merchant queries.
    pub merchant_id: Option<Uuid>,

    /// Short machine-readable event name, e.g. `"batch_identified"`.
    pub event_name: String,

    /// Structured JSON detail. MUST NOT contain secrets. Typical fields:
    /// `wallet_id`, `amount_stroops`, `asset`, `error_category`, `tx_hash`.
    pub detail: JsonValue,

    /// Severity: `"info"`, `"warn"`, or `"error"`.
    pub severity: String,

    pub occurred_at: DateTime<Utc>,
}

/// The valid severity levels for [`AuditLogEntry::severity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
}

impl AuditSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditSeverity::Info => "info",
            AuditSeverity::Warn => "warn",
            AuditSeverity::Error => "error",
        }
    }
}

impl std::fmt::Display for AuditSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Input for inserting a new audit log entry.
///
/// Callers must ensure `detail` contains no secrets.
#[derive(Debug, Clone)]
pub struct NewAuditLogEntry {
    pub sweep_batch_id: Option<Uuid>,
    pub redemption_attempt_id: Option<Uuid>,
    pub merchant_id: Option<Uuid>,
    pub event_name: String,
    /// Structured, secrets-free JSON detail.
    pub detail: JsonValue,
    pub severity: AuditSeverity,
}

// ---------------------------------------------------------------------------
// Well-known event name constants
// ---------------------------------------------------------------------------
// Using constants prevents typos and makes event names greppable across the
// codebase without stringly-typed magic strings scattered everywhere.

pub mod event {
    /// Eligible funds identified; a sweep_batch row was created.
    pub const BATCH_IDENTIFIED: &str = "batch_identified";
    /// Skipped — an idempotency_key conflict means this batch already exists.
    pub const BATCH_SKIPPED_DUPLICATE: &str = "batch_skipped_duplicate";
    /// Stellar tx constructed; hash and XDR stored before submission.
    pub const SWEEP_STARTED: &str = "sweep_started";
    /// Stellar tx submitted to Horizon.
    pub const SWEEP_SUBMITTED: &str = "sweep_submitted";
    /// Horizon confirmed the sweep transaction.
    pub const SWEEP_CONFIRMED: &str = "sweep_confirmed";
    /// On-chain sweep failed.
    pub const SWEEP_FAILED: &str = "sweep_failed";
    /// Issuer redemption API call about to be made.
    pub const REDEMPTION_STARTED: &str = "redemption_started";
    /// Issuer confirmed NGN credited.
    pub const REDEMPTION_SUCCEEDED: &str = "redemption_succeeded";
    /// Issuer API call failed for this attempt.
    pub const REDEMPTION_FAILED: &str = "redemption_failed";
    /// Max retries exceeded; ops alert raised.
    pub const TERMINAL_FAILURE: &str = "terminal_failure";
    /// Entire pipeline complete; batch is ready_for_payout.
    pub const BATCH_COMPLETED: &str = "batch_completed";
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_status_round_trips() {
        let statuses = [
            SweepStatus::Identified,
            SweepStatus::Sweeping,
            SweepStatus::Swept,
            SweepStatus::Redeeming,
            SweepStatus::Redeemed,
            SweepStatus::ReadyForPayout,
            SweepStatus::SweepFailed,
            SweepStatus::RedemptionFailed,
            SweepStatus::TerminalFailed,
        ];
        for status in statuses {
            let s = status.as_str();
            let parsed = SweepStatus::try_from(s).expect("parse failed");
            assert_eq!(parsed, status, "round-trip failed for {s}");
        }
    }

    #[test]
    fn redemption_status_round_trips() {
        let statuses = [
            RedemptionStatus::Pending,
            RedemptionStatus::Submitted,
            RedemptionStatus::Succeeded,
            RedemptionStatus::Failed,
        ];
        for status in statuses {
            let s = status.as_str();
            let parsed = RedemptionStatus::try_from(s).expect("parse failed");
            assert_eq!(parsed, status, "round-trip failed for {s}");
        }
    }

    #[test]
    fn audit_severity_as_str_is_lowercase() {
        assert_eq!(AuditSeverity::Info.as_str(), "info");
        assert_eq!(AuditSeverity::Warn.as_str(), "warn");
        assert_eq!(AuditSeverity::Error.as_str(), "error");
    }

    #[test]
    fn sweep_status_unknown_str_errors() {
        let result = SweepStatus::try_from("not_a_real_status");
        assert!(result.is_err(), "expected error for unknown status");
        let msg = result.unwrap_err();
        assert!(msg.contains("not_a_real_status"));
    }

    #[test]
    fn new_audit_log_entry_detail_is_structured() {
        // Verifies that detail accepts arbitrary valid JSON — important for the
        // secrets-safety guarantee: callers produce structured JSON, not raw strings.
        let entry = NewAuditLogEntry {
            sweep_batch_id: Some(Uuid::new_v4()),
            redemption_attempt_id: None,
            merchant_id: Some(Uuid::new_v4()),
            event_name: event::BATCH_IDENTIFIED.to_string(),
            detail: serde_json::json!({
                "wallet_id": "some-uuid",
                "amount_stroops": 5_000_000_i64,
                "asset": "cNGN"
            }),
            severity: AuditSeverity::Info,
        };
        // detail must be a JSON object (not a raw string with key material)
        assert!(entry.detail.is_object());
        // confirm no accidental secret field names
        let obj = entry.detail.as_object().unwrap();
        assert!(!obj.contains_key("secret_key"));
        assert!(!obj.contains_key("private_key"));
        assert!(!obj.contains_key("seed"));
    }

    #[test]
    fn event_name_constants_are_snake_case() {
        // Guard against accidental camelCase or spaces in event names
        let constants = [
            event::BATCH_IDENTIFIED,
            event::BATCH_SKIPPED_DUPLICATE,
            event::SWEEP_STARTED,
            event::SWEEP_SUBMITTED,
            event::SWEEP_CONFIRMED,
            event::SWEEP_FAILED,
            event::REDEMPTION_STARTED,
            event::REDEMPTION_SUCCEEDED,
            event::REDEMPTION_FAILED,
            event::TERMINAL_FAILURE,
            event::BATCH_COMPLETED,
        ];
        for name in constants {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "event name '{name}' is not snake_case"
            );
        }
    }
}
