use async_trait::async_trait;

use super::{PaymentProvider, PayoutReadiness, PayoutRequest, PayoutResult};

pub struct MockProvider;

#[async_trait]
impl PaymentProvider for MockProvider {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
        Ok(PayoutReadiness {
            is_ready: true,
            available_balance: Some(1_000_000_000), // Simulated 10,000,000 NGN
            message: "Mock provider is always ready".into(),
        })
    }

    async fn create_payout(&self, req: &PayoutRequest) -> Result<PayoutResult, String> {
        Ok(PayoutResult {
            provider: "mock".into(),
            provider_reference: format!("mock_{}", req.reference),
            status: "pending".into(),
        })
    }
}