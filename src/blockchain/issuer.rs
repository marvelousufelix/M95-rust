/// Issuer integration: defines the interface for cNGN redemption API.
///
/// This module provides the trait and data structures for communicating with the
/// approved cNGN issuer. The actual HTTP implementation is in the worker orchestration
/// layer (settlement_worker.rs), which handles retries and error categorization.
///
/// # Secrets Safety
/// Issuer responses are parsed carefully to extract only necessary fields.
/// Error messages are never logged verbatim — only categorised.

use serde::{Deserialize, Serialize};

/// Request sent to the issuer's redemption API.
///
/// This is the payload structure sent to the issuer when converting cNGN → NGN.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedemptionRequest {
    /// Unique identifier for this redemption request (for deduplication).
    /// Format: `sha256(sweep_batch_id || attempt_number)`.
    pub idempotency_key: String,

    /// The amount in cNGN stroops to redeem.
    pub amount_stroops: i64,

    /// The merchant ID or account reference in the issuer's system.
    /// Optional; may be included for reconciliation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merchant_reference: Option<String>,

    /// Optional memo or context for the issuer's records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// Response from a successful issuer redemption.
///
/// Indicates that the issuer has accepted the redemption and credited NGN
/// to the platform account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedemptionSuccess {
    /// Reference assigned by the issuer (e.g., transaction ID).
    pub reference: String,

    /// The amount of NGN (in kobo, Nigeria's minor unit) that was credited.
    /// Example: 1000 kobo = 10 NGN.
    pub ngn_amount_kobo: i64,

    /// Optional: issuer-assigned memo or status message.
    #[serde(default)]
    pub memo: Option<String>,
}

/// Error response from the issuer's redemption API.
///
/// Captures both structured error responses and fallback information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedemptionError {
    /// Error code from the issuer (e.g., "INVALID_AMOUNT", "ACCOUNT_SUSPENDED").
    pub code: String,

    /// Human-readable error message from the issuer.
    pub message: String,

    /// Whether this error is retryable (transient) or terminal.
    /// Transient: network timeout, issuer temporarily unavailable, rate-limited.
    /// Terminal: invalid amount, unsupported asset, account closed.
    #[serde(default)]
    pub retryable: bool,

    /// Optional: issuer-assigned reference even on failure (for reconciliation).
    #[serde(default)]
    pub reference: Option<String>,
}

impl RedemptionError {
    /// Determine if this error is retryable based on the error code.
    ///
    /// This is a heuristic; the issuer API contract may provide explicit
    /// `retryable` field. If not present, we infer from known error codes.
    pub fn infer_retryable(&self) -> bool {
        // Terminal error codes (not retryable)
        let terminal_codes = [
            "INVALID_AMOUNT",
            "UNSUPPORTED_ASSET",
            "ACCOUNT_NOT_FOUND",
            "ACCOUNT_SUSPENDED",
            "ACCOUNT_CLOSED",
            "PERMISSION_DENIED",
            "DUPLICATE_REFERENCE", // already processed
        ];

        // If the issuer provided an explicit flag, use it.
        if self.retryable {
            return true;
        }

        // Otherwise, if the code is in the terminal list, don't retry.
        !terminal_codes.contains(&self.code.as_str())
    }
}

/// The outcome of an attempted redemption with the issuer.
#[derive(Debug, Clone)]
pub enum RedemptionOutcome {
    /// Redemption succeeded; NGN was credited to the platform account.
    Success(RedemptionSuccess),

    /// Redemption failed with a transient error; eligible for retry.
    RetryableFailure {
        error: RedemptionError,
        /// Suggested backoff duration before retrying (if provided by issuer).
        backoff_secs: Option<u32>,
    },

    /// Redemption failed with a terminal error; requires ops intervention.
    TerminalFailure(RedemptionError),

    /// Network error (request not completed; unknown outcome).
    /// Treat as transient — the issuer may have processed it.
    NetworkError(String),
}

impl RedemptionOutcome {
    /// Categorise this outcome for logging and retry decisions.
    pub fn category(&self) -> &'static str {
        match self {
            RedemptionOutcome::Success(_) => "success",
            RedemptionOutcome::RetryableFailure { .. } => "retryable_failure",
            RedemptionOutcome::TerminalFailure(_) => "terminal_failure",
            RedemptionOutcome::NetworkError(_) => "network_error",
        }
    }

    /// Check if this outcome indicates the redemption is complete (not pending).
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            RedemptionOutcome::Success(_) | RedemptionOutcome::TerminalFailure(_)
        )
    }

    /// Extract an optional reference from the outcome (for audit trail).
    pub fn reference(&self) -> Option<&str> {
        match self {
            RedemptionOutcome::Success(s) => Some(&s.reference),
            RedemptionOutcome::RetryableFailure { error, .. } => error.reference.as_deref(),
            RedemptionOutcome::TerminalFailure(error) => error.reference.as_deref(),
            RedemptionOutcome::NetworkError(_) => None,
        }
    }
}

/// Trait for issuer integration implementations.
///
/// This trait abstracts the issuer API so the settlement worker can be tested
/// with mock implementations and easily swapped for new issuers.
#[async_trait::async_trait]
pub trait IssuerClient: Send + Sync {
    /// Submit a redemption request to the issuer.
    /// Returns the outcome (success, retryable failure, terminal failure, or network error).
    async fn redeem(
        &self,
        request: RedemptionRequest,
    ) -> RedemptionOutcome;

    /// Query the status of a previous redemption attempt.
    /// Used to poll for completion when the initial request was submitted but not confirmed.
    ///
    /// # Arguments
    /// - `idempotency_key`: The key used in the original redemption request.
    ///
    /// # Returns
    /// `Ok(Some(outcome))` if the issuer has a record of this redemption.
    /// `Ok(None)` if the issuer has no record (meaning it was not received).
    /// `Err(msg)` if the query itself failed (treat as transient).
    async fn query_redemption_status(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<RedemptionOutcome>, String>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redemption_error_infers_terminal_for_invalid_amount() {
        let error = RedemptionError {
            code: "INVALID_AMOUNT".to_string(),
            message: "Amount exceeds limit".to_string(),
            retryable: false,
            reference: None,
        };
        assert!(!error.infer_retryable(), "invalid amount should not be retryable");
    }

    #[test]
    fn redemption_error_infers_terminal_for_account_closed() {
        let error = RedemptionError {
            code: "ACCOUNT_CLOSED".to_string(),
            message: "Account is closed".to_string(),
            retryable: false,
            reference: None,
        };
        assert!(!error.infer_retryable(), "account closed should not be retryable");
    }

    #[test]
    fn redemption_error_infers_retryable_for_unknown_code() {
        let error = RedemptionError {
            code: "UNKNOWN_ERROR_CODE".to_string(),
            message: "Something went wrong".to_string(),
            retryable: false,
            reference: None,
        };
        assert!(error.infer_retryable(), "unknown codes should be treated as retryable");
    }

    #[test]
    fn redemption_error_respects_explicit_retryable_flag() {
        let error = RedemptionError {
            code: "INVALID_AMOUNT".to_string(), // would normally be terminal
            message: "Amount exceeds limit".to_string(),
            retryable: true, // explicit override
            reference: None,
        };
        assert!(error.infer_retryable(), "explicit retryable flag should take precedence");
    }

    #[test]
    fn redemption_outcome_category() {
        let success = RedemptionOutcome::Success(RedemptionSuccess {
            reference: "REF123".to_string(),
            ngn_amount_kobo: 1000,
            memo: None,
        });
        assert_eq!(success.category(), "success");

        let network_err = RedemptionOutcome::NetworkError("timeout".to_string());
        assert_eq!(network_err.category(), "network_error");

        let terminal = RedemptionOutcome::TerminalFailure(RedemptionError {
            code: "ACCOUNT_CLOSED".to_string(),
            message: "Account closed".to_string(),
            retryable: false,
            reference: None,
        });
        assert_eq!(terminal.category(), "terminal_failure");
    }

    #[test]
    fn redemption_outcome_is_final() {
        let success = RedemptionOutcome::Success(RedemptionSuccess {
            reference: "REF123".to_string(),
            ngn_amount_kobo: 1000,
            memo: None,
        });
        assert!(success.is_final());

        let network_err = RedemptionOutcome::NetworkError("timeout".to_string());
        assert!(!network_err.is_final(), "network error is not final");

        let retryable = RedemptionOutcome::RetryableFailure {
            error: RedemptionError {
                code: "TIMEOUT".to_string(),
                message: "Request timed out".to_string(),
                retryable: true,
                reference: None,
            },
            backoff_secs: None,
        };
        assert!(!retryable.is_final(), "retryable failure is not final");
    }
}
