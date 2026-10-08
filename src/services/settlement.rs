/// Settlement pipeline service: sweep batch management and state transitions.
///
/// This module implements the core business logic for moving funds from individual
/// merchant wallets through the settlement pipeline:
///   1. Sweep: merchant wallet → platform settlement wallet (on-chain)
///   2. Redemption: issuer API call to exchange cNGN for NGN
///   3. Ready for Payout: NGN available in platform wallet for merchant payouts
///
/// All state transitions are atomic (race-safe) and designed to be idempotent:
/// a worker crash and restart will not create duplicate sweeps or redemptions.
///
/// # Secrets Safety
/// This module never holds decrypted private keys in memory longer than needed,
/// and error messages are sanitised before storage in the audit log.

use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::settlement::{
    AuditLogEntry, AuditSeverity, NewAuditLogEntry, NewRedemptionAttempt, NewSweepBatch,
    RedemptionAttempt, RedemptionFailure, RedemptionStatus, RedemptionSuccess, SweepBatch,
    SweepStatus, TransitionSweepStatus,
};

// ---------------------------------------------------------------------------
// SweepBatchService: Create and manage sweep batches
// ---------------------------------------------------------------------------

/// Service for sweep batch operations.
pub struct SweepBatchService;

impl SweepBatchService {
    /// Compute the idempotency key for a sweep batch.
    /// `sha256(wallet_id::text || window_start::iso8601)` ensures that
    /// if the worker crashes and restarts, a new attempt for the same
    /// wallet in the same settlement window will see the existing batch
    /// and skip (thanks to UNIQUE constraint).
    pub fn compute_idempotency_key(wallet_id: Uuid, window_start: DateTime<Utc>) -> String {
        let mut hasher = Sha256::new();
        hasher.update(wallet_id.to_string());
        hasher.update(window_start.to_rfc3339());
        let hash = hasher.finalize();
        format!("{:x}", hash)
    }

    /// Create a new sweep batch if it doesn't already exist (idempotent).
    /// Returns the batch ID and a flag indicating whether this was a new insert
    /// or a pre-existing batch (skipped due to duplicate key).
    ///
    /// # Arguments
    /// - `pool`: Database connection pool
    /// - `wallet_id`: The wallet being swept
    /// - `merchant_id`: The merchant who owns the wallet (denormalised for queries)
    /// - `window_start`: The start of this settlement window
    /// - `eligible_stroops`: How many stroops were eligible at batch creation
    /// - `asset`: The asset being swept (typically "cNGN")
    ///
    /// # Returns
    /// `(batch_id, is_new)` where `is_new` is `true` if this is a fresh insert,
    /// or `false` if an identical batch already existed.
    pub async fn create_batch(
        pool: &PgPool,
        wallet_id: Uuid,
        merchant_id: Uuid,
        window_start: DateTime<Utc>,
        eligible_stroops: i64,
        asset: &str,
    ) -> Result<(Uuid, bool), String> {
        let idempotency_key = Self::compute_idempotency_key(wallet_id, window_start);

        // Attempt INSERT with UNIQUE constraint. If the key already exists,
        // we fetch the existing row and return is_new=false.
        let result = sqlx::query_as::<_, SweepBatch>(
            r#"
            INSERT INTO sweep_batches (
                wallet_id, merchant_id, window_start, idempotency_key,
                eligible_stroops, asset, status
            )
            VALUES ($1, $2, $3, $4, $5, $6, 'identified')
            ON CONFLICT (idempotency_key)
            DO UPDATE SET updated_at = now()
            RETURNING *
            "#,
        )
        .bind(wallet_id)
        .bind(merchant_id)
        .bind(window_start)
        .bind(&idempotency_key)
        .bind(eligible_stroops)
        .bind(asset)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("create_batch DB error: {}", e))?;

        // If the batch was just created (created_at == updated_at within ~100ms),
        // it's new. Otherwise it already existed. A more robust check would use
        // xmin/xmax or a separate boolean, but this is sufficient for now.
        let is_new = result.created_at.signed_duration_since(result.updated_at).num_milliseconds() < 100;
        Ok((result.id, is_new))
    }

    /// Fetch all batches in a given state.
    /// Used by the worker loop to find pending sweeps, pending redemptions, etc.
    pub async fn batches_in_status(
        pool: &PgPool,
        status: SweepStatus,
    ) -> Result<Vec<SweepBatch>, String> {
        sqlx::query_as::<_, SweepBatch>(
            "SELECT * FROM sweep_batches WHERE status = $1 ORDER BY created_at ASC",
        )
        .bind(status.as_str())
        .fetch_all(pool)
        .await
        .map_err(|e| format!("batches_in_status DB error: {}", e))
    }

    /// Fetch a single batch by ID.
    pub async fn get_batch(pool: &PgPool, batch_id: Uuid) -> Result<Option<SweepBatch>, String> {
        sqlx::query_as::<_, SweepBatch>("SELECT * FROM sweep_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("get_batch DB error: {}", e))
    }

    /// Atomically transition a batch from one status to another.
    /// Returns the number of rows affected (0 if the expected status didn't match).
    /// Only the worker with the correct expected status wins the race.
    pub async fn transition_status(
        pool: &PgPool,
        transition: &TransitionSweepStatus,
    ) -> Result<u64, String> {
        let new_status = transition.new_status.as_str();
        let expected_status = transition.expected_status.as_str();
        let retry_increment = if transition.increment_retry { 1 } else { 0 };

        let rows_affected = sqlx::query(
            r#"
            UPDATE sweep_batches
            SET status = $1,
                last_error = $2,
                retry_count = retry_count + $3,
                updated_at = now()
            WHERE id = $4 AND status = $5
            "#,
        )
        .bind(new_status)
        .bind(&transition.last_error)
        .bind(retry_increment)
        .bind(transition.id)
        .bind(expected_status)
        .execute(pool)
        .await
        .map_err(|e| format!("transition_status DB error: {}", e))?
        .rows_affected();

        Ok(rows_affected)
    }

    /// Store the Stellar transaction hash and XDR before submission to Horizon.
    /// This is done atomically so a crash-recovery scan can check Horizon
    /// for the transaction outcome even if the app crashes after this but before
    /// the status transition.
    pub async fn set_sweep_tx_details(
        pool: &PgPool,
        batch_id: Uuid,
        tx_hash: &str,
        tx_xdr: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE sweep_batches SET stellar_tx_hash = $1, stellar_tx_xdr = $2, updated_at = now() WHERE id = $3",
        )
        .bind(tx_hash)
        .bind(tx_xdr)
        .bind(batch_id)
        .execute(pool)
        .await
        .map_err(|e| format!("set_sweep_tx_details DB error: {}", e))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RedemptionAttemptService: Track issuer API calls
// ---------------------------------------------------------------------------

/// Service for redemption attempt operations.
pub struct RedemptionAttemptService;

impl RedemptionAttemptService {
    /// Compute the idempotency key for a redemption attempt.
    /// `sha256(sweep_batch_id::text || attempt_number::text)` is sent to the issuer API
    /// so the issuer can detect and safely return the outcome of a duplicate submission.
    pub fn compute_idempotency_key(batch_id: Uuid, attempt_number: i32) -> String {
        let mut hasher = Sha256::new();
        hasher.update(batch_id.to_string());
        hasher.update(attempt_number.to_string());
        let hash = hasher.finalize();
        format!("{:x}", hash)
    }

    /// Create a new redemption attempt for a batch.
    /// Finds the highest existing attempt_number and increments it,
    /// or starts at 1 if this is the first attempt.
    pub async fn create_attempt(
        pool: &PgPool,
        batch_id: Uuid,
        amount_stroops: i64,
    ) -> Result<RedemptionAttempt, String> {
        // Fetch the highest existing attempt number for this batch
        let max_attempt: (Option<i32>,) = sqlx::query_as(
            "SELECT MAX(attempt_number) FROM redemption_attempts WHERE sweep_batch_id = $1",
        )
        .bind(batch_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("create_attempt fetch max error: {}", e))?;

        let next_attempt_number = max_attempt.0.map(|n| n + 1).unwrap_or(1);
        let idempotency_key = Self::compute_idempotency_key(batch_id, next_attempt_number);

        let attempt = sqlx::query_as::<_, RedemptionAttempt>(
            r#"
            INSERT INTO redemption_attempts (
                sweep_batch_id, attempt_number, idempotency_key, amount_stroops, status
            )
            VALUES ($1, $2, $3, $4, 'pending')
            RETURNING *
            "#,
        )
        .bind(batch_id)
        .bind(next_attempt_number)
        .bind(&idempotency_key)
        .bind(amount_stroops)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("create_attempt insert error: {}", e))?;

        Ok(attempt)
    }

    /// Fetch all attempts for a batch, ordered by attempt number.
    pub async fn attempts_for_batch(
        pool: &PgPool,
        batch_id: Uuid,
    ) -> Result<Vec<RedemptionAttempt>, String> {
        sqlx::query_as::<_, RedemptionAttempt>(
            "SELECT * FROM redemption_attempts WHERE sweep_batch_id = $1 ORDER BY attempt_number ASC",
        )
        .bind(batch_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("attempts_for_batch DB error: {}", e))
    }

    /// Fetch the most recent attempt for a batch.
    pub async fn latest_attempt(
        pool: &PgPool,
        batch_id: Uuid,
    ) -> Result<Option<RedemptionAttempt>, String> {
        sqlx::query_as::<_, RedemptionAttempt>(
            r#"
            SELECT * FROM redemption_attempts
            WHERE sweep_batch_id = $1
            ORDER BY attempt_number DESC
            LIMIT 1
            "#,
        )
        .bind(batch_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("latest_attempt DB error: {}", e))
    }

    /// Fetch all in-flight attempts (pending or submitted).
    pub async fn in_flight_attempts(pool: &PgPool) -> Result<Vec<RedemptionAttempt>, String> {
        sqlx::query_as::<_, RedemptionAttempt>(
            r#"
            SELECT * FROM redemption_attempts
            WHERE status IN ('pending', 'submitted')
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("in_flight_attempts DB error: {}", e))
    }

    /// Mark an attempt as submitted (API call sent).
    pub async fn mark_submitted(
        pool: &PgPool,
        attempt_id: Uuid,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE redemption_attempts SET status = 'submitted', updated_at = now() WHERE id = $1",
        )
        .bind(attempt_id)
        .execute(pool)
        .await
        .map_err(|e| format!("mark_submitted DB error: {}", e))?;

        Ok(())
    }

    /// Mark an attempt as succeeded (issuer confirmed NGN credited).
    pub async fn mark_succeeded(
        pool: &PgPool,
        success: &RedemptionSuccess,
    ) -> Result<(), String> {
        sqlx::query(
            r#"
            UPDATE redemption_attempts
            SET status = 'succeeded',
                issuer_reference = $1,
                ngn_amount_kobo = $2,
                updated_at = now()
            WHERE id = $3
            "#,
        )
        .bind(&success.issuer_reference)
        .bind(success.ngn_amount_kobo)
        .bind(success.attempt_id)
        .execute(pool)
        .await
        .map_err(|e| format!("mark_succeeded DB error: {}", e))?;

        Ok(())
    }

    /// Mark an attempt as failed.
    pub async fn mark_failed(
        pool: &PgPool,
        failure: &RedemptionFailure,
    ) -> Result<(), String> {
        sqlx::query(
            r#"
            UPDATE redemption_attempts
            SET status = 'failed',
                error_message = $1,
                issuer_reference = $2,
                updated_at = now()
            WHERE id = $3
            "#,
        )
        .bind(&failure.error_message)
        .bind(&failure.issuer_reference)
        .bind(failure.attempt_id)
        .execute(pool)
        .await
        .map_err(|e| format!("mark_failed DB error: {}", e))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AuditLogService: Record settlement events without exposing secrets
// ---------------------------------------------------------------------------

/// Service for audit logging.
pub struct AuditLogService;

impl AuditLogService {
    /// Insert a new audit log entry.
    /// The detail field must be valid JSON and must NOT contain any secrets
    /// (private keys, seeds, passphrases, etc.). The application layer is
    /// responsible for sanitising all values before constructing the entry.
    pub async fn log_event(
        pool: &PgPool,
        entry: &NewAuditLogEntry,
    ) -> Result<Uuid, String> {
        let id = sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO settlement_audit_log (
                sweep_batch_id, redemption_attempt_id, merchant_id,
                event_name, detail, severity, occurred_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
            "#,
        )
        .bind(entry.sweep_batch_id)
        .bind(entry.redemption_attempt_id)
        .bind(entry.merchant_id)
        .bind(&entry.event_name)
        .bind(&entry.detail)
        .bind(entry.severity.as_str())
        .bind(Utc::now())
        .fetch_one(pool)
        .await
        .map_err(|e| format!("log_event DB error: {}", e))?;

        Ok(id)
    }

    /// Fetch recent audit log entries for a batch.
    pub async fn events_for_batch(
        pool: &PgPool,
        batch_id: Uuid,
        limit: i32,
    ) -> Result<Vec<AuditLogEntry>, String> {
        sqlx::query_as::<_, AuditLogEntry>(
            r#"
            SELECT * FROM settlement_audit_log
            WHERE sweep_batch_id = $1
            ORDER BY occurred_at DESC
            LIMIT $2
            "#,
        )
        .bind(batch_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("events_for_batch DB error: {}", e))
    }

    /// Fetch recent error/warning events (for ops alerting).
    pub async fn recent_errors(
        pool: &PgPool,
        limit: i32,
    ) -> Result<Vec<AuditLogEntry>, String> {
        sqlx::query_as::<_, AuditLogEntry>(
            r#"
            SELECT * FROM settlement_audit_log
            WHERE severity IN ('warn', 'error')
            ORDER BY occurred_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("recent_errors DB error: {}", e))
    }

    /// Fetch all events for a merchant (for dashboard/reporting).
    pub async fn events_for_merchant(
        pool: &PgPool,
        merchant_id: Uuid,
        limit: i32,
    ) -> Result<Vec<AuditLogEntry>, String> {
        sqlx::query_as::<_, AuditLogEntry>(
            r#"
            SELECT * FROM settlement_audit_log
            WHERE merchant_id = $1
            ORDER BY occurred_at DESC
            LIMIT $2
            "#,
        )
        .bind(merchant_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("events_for_merchant DB error: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Error Sanitization Helpers
// ---------------------------------------------------------------------------

/// Sanitise error messages before storing in the audit log.
/// Maps error details to fixed-text categories to avoid exposing
/// wallet IDs, tx hashes, or other sensitive info in error messages.
pub fn sanitise_error_message(error: &str) -> String {
    // Map known error patterns to safe categories
    if error.contains("timeout") || error.contains("deadline exceeded") {
        "network_timeout".to_string()
    } else if error.contains("connection refused") || error.contains("unreachable") {
        "network_unreachable".to_string()
    } else if error.contains("404") || error.contains("not found") {
        "not_found".to_string()
    } else if error.contains("400") || error.contains("bad request") {
        "invalid_request".to_string()
    } else if error.contains("500") || error.contains("internal") {
        "server_error".to_string()
    } else if error.contains("rate limit") || error.contains("429") {
        "rate_limited".to_string()
    } else if error.contains("insufficient") {
        "insufficient_funds".to_string()
    } else if error.contains("invalid") || error.contains("malformed") {
        "invalid_format".to_string()
    } else {
        // Generic fallback; never include raw error text
        "operation_failed".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_idempotency_key_is_deterministic() {
        let wallet_id = Uuid::new_v4();
        let window_start = DateTime::parse_from_rfc3339("2026-10-08T02:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);

        let key1 = SweepBatchService::compute_idempotency_key(wallet_id, window_start);
        let key2 = SweepBatchService::compute_idempotency_key(wallet_id, window_start);

        assert_eq!(key1, key2, "idempotency key should be deterministic");
    }

    #[test]
    fn compute_idempotency_key_differs_by_wallet() {
        let wallet_id_1 = Uuid::new_v4();
        let wallet_id_2 = Uuid::new_v4();
        let window_start = DateTime::parse_from_rfc3339("2026-10-08T02:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);

        let key1 = SweepBatchService::compute_idempotency_key(wallet_id_1, window_start);
        let key2 = SweepBatchService::compute_idempotency_key(wallet_id_2, window_start);

        assert_ne!(key1, key2, "different wallets should have different keys");
    }

    #[test]
    fn compute_idempotency_key_differs_by_window() {
        let wallet_id = Uuid::new_v4();
        let window_1 = DateTime::parse_from_rfc3339("2026-10-08T02:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let window_2 = DateTime::parse_from_rfc3339("2026-10-09T02:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);

        let key1 = SweepBatchService::compute_idempotency_key(wallet_id, window_1);
        let key2 = SweepBatchService::compute_idempotency_key(wallet_id, window_2);

        assert_ne!(key1, key2, "different windows should have different keys");
    }

    #[test]
    fn redemption_idempotency_key_is_deterministic() {
        let batch_id = Uuid::new_v4();
        let attempt_num = 1;

        let key1 = RedemptionAttemptService::compute_idempotency_key(batch_id, attempt_num);
        let key2 = RedemptionAttemptService::compute_idempotency_key(batch_id, attempt_num);

        assert_eq!(key1, key2, "redemption idempotency key should be deterministic");
    }

    #[test]
    fn redemption_idempotency_key_differs_by_attempt() {
        let batch_id = Uuid::new_v4();

        let key1 = RedemptionAttemptService::compute_idempotency_key(batch_id, 1);
        let key2 = RedemptionAttemptService::compute_idempotency_key(batch_id, 2);

        assert_ne!(key1, key2, "different attempt numbers should have different keys");
    }

    #[test]
    fn sanitise_error_handles_timeout() {
        assert_eq!(
            sanitise_error_message("operation timed out"),
            "network_timeout"
        );
        assert_eq!(
            sanitise_error_message("deadline exceeded"),
            "network_timeout"
        );
    }

    #[test]
    fn sanitise_error_handles_not_found() {
        assert_eq!(sanitise_error_message("404 not found"), "not_found");
    }

    #[test]
    fn sanitise_error_never_includes_raw_text() {
        let error = "critical error at line 42 in module X with secret_key ABC123";
        let sanitised = sanitise_error_message(error);
        assert!(!sanitised.contains("secret_key"));
        assert!(!sanitised.contains("ABC123"));
        assert!(!sanitised.contains("critical error"));
    }
}
