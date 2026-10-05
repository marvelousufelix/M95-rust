pub mod mock;
pub mod paystack;

use async_trait::async_trait;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PayoutRequest {
    pub bank_code: String,
    pub account_number: String,
    /// Smallest currency unit for the payout rail (e.g. kobo for a Naira payout).
    pub amount: String,
    pub reference: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PayoutResult {
    pub provider: String,
    pub provider_reference: String,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PayoutReadiness {
    /// Whether the provider has sufficient funding to process a payout.
    pub is_ready: bool,
    /// The available balance (in smallest currency unit) on the provider account.
    /// Useful for logging and debugging.
    pub available_balance: Option<i64>,
    /// Human-readable message explaining readiness status.
    pub message: String,
}

#[async_trait]
pub trait PaymentProvider: Send + Sync {
    /// Check whether the provider has sufficient funding to process payouts.
    /// This is a pre-flight check to prevent user requests from being debited
    /// when no payout is possible.
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String>;

    /// Initiate a payout to a specific recipient. Called only after readiness is verified.
    async fn create_payout(&self, req: &PayoutRequest) -> Result<PayoutResult, String>;
}