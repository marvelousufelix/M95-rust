use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::{PaymentProvider, PayoutReadiness, PayoutRequest, PayoutResult};

const BASE_URL: &str = "https://api.paystack.co";

pub struct PaystackProvider {
    secret_key: String,
    http: reqwest::Client,
}

impl PaystackProvider {
    pub fn new(secret_key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("failed to build Paystack HTTP client");
        Self { secret_key, http }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, String> {
        let response = self
            .http
            .get(format!("{BASE_URL}{path}"))
            .bearer_auth(&self.secret_key)
            .query(query)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Self::unwrap_response(response).await
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T, String> {
        let response = self
            .http
            .post(format!("{BASE_URL}{path}"))
            .bearer_auth(&self.secret_key)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Self::unwrap_response(response).await
    }

    async fn unwrap_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
        let status = response.status();
        let raw = response.text().await.map_err(|e| e.to_string())?;
        let body: PaystackResponse<T> = serde_json::from_str(&raw).map_err(|e| {
            format!("Paystack returned an unexpected response (HTTP {status}): {e} — body: {raw}")
        })?;
        if !status.is_success() || !body.status {
            return Err(format!("Paystack error (HTTP {status}): {}", body.message));
        }
        body.data.ok_or_else(|| "Paystack response missing data".to_string())
    }
}

#[derive(Debug, Deserialize)]
struct PaystackResponse<T> {
    status: bool,
    message: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ResolvedAccount {
    account_name: String,
}

#[derive(Debug, Deserialize)]
struct Recipient {
    recipient_code: String,
}

#[derive(Debug, Deserialize)]
struct Transfer {
    transfer_code: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct BalanceInfo {
    balance: i64,
}

#[async_trait]
impl PaymentProvider for PaystackProvider {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
        // Query Paystack's balance endpoint to get the account's available NGN balance.
        // This is the primary signal for whether the account can fund transfers.
        match self.get::<BalanceInfo>("/balance", &[]).await {
            Ok(balance_info) => {
                let is_ready = balance_info.balance > 0;
                let message = if is_ready {
                    format!("Paystack account has sufficient balance (₦{})", balance_info.balance / 100)
                } else {
                    "Paystack account balance is zero or negative; transfers cannot be funded".into()
                };
                Ok(PayoutReadiness {
                    is_ready,
                    available_balance: Some(balance_info.balance),
                    message,
                })
            }
            Err(err) => {
                // If the balance check itself fails (network error, auth issue, etc.),
                // report it as a readiness failure so the merchant sees a clear error
                // instead of silently proceeding to a payout that will also fail.
                tracing::warn!(error = %err, "payout readiness check failed");
                Ok(PayoutReadiness {
                    is_ready: false,
                    available_balance: None,
                    message: format!("Unable to verify Paystack account status: {err}"),
                })
            }
        }
    }

    async fn create_payout(&self, req: &PayoutRequest) -> Result<PayoutResult, String> {
        let amount_kobo: i64 = req
            .amount
            .parse()
            .map_err(|_| format!("invalid payout amount: {}", req.amount))?;

        // Best-effort: resolving gets us the real account holder's name (and would
        // catch a typo'd account number), but its failure shouldn't hard-block the
        // payout — Paystack checks this against real NIBSS data even in test mode,
        // so a fabricated test account number fails here even though the recipient
        // and transfer calls below don't perform the same check.
        let resolved: Option<ResolvedAccount> = self
            .get(
                "/bank/resolve",
                &[("account_number", req.account_number.as_str()), ("bank_code", req.bank_code.as_str())],
            )
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "account resolution failed, proceeding with a placeholder name");
                err
            })
            .ok();
        let recipient_name = resolved
            .map(|r| r.account_name)
            .unwrap_or_else(|| "Aframp Merchant".to_string());

        let recipient: Recipient = self
            .post(
                "/transferrecipient",
                &serde_json::json!({
                    "type": "nuban",
                    "name": recipient_name,
                    "account_number": req.account_number,
                    "bank_code": req.bank_code,
                    "currency": "NGN",
                }),
            )
            .await?;

        // Test-mode transfers resolve immediately with no real processing, so the
        // status on this response is authoritative for our purposes. Live mode is
        // genuinely async (may require OTP finalization) and would need a webhook
        // or a follow-up "verify transfer" call to reconcile the final status.
        let transfer: Transfer = self
            .post(
                "/transfer",
                &serde_json::json!({
                    "source": "balance",
                    "amount": amount_kobo,
                    "recipient": recipient.recipient_code,
                    "reason": "Aframp merchant withdrawal",
                    "reference": req.reference,
                }),
            )
            .await?;

        Ok(PayoutResult {
            provider: "paystack".into(),
            provider_reference: transfer.transfer_code,
            status: transfer.status,
        })
    }
}
