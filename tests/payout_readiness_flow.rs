mod common;

use std::sync::Arc;

use aframp::payments::{PaymentProvider, PayoutReadiness, PayoutRequest, PayoutResult};
use async_trait::async_trait;
use axum::http::StatusCode;
use serde_json::json;

use common::{ensure_merchant, send, state};

/// Mock provider that simulates Paystack being funded and ready
struct ReadyProvider;

#[async_trait]
impl PaymentProvider for ReadyProvider {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
        Ok(PayoutReadiness {
            is_ready: true,
            available_balance: Some(100_000_000), // 1,000,000 NGN
            message: "Mock provider has sufficient balance".into(),
        })
    }

    async fn create_payout(&self, _req: &PayoutRequest) -> Result<PayoutResult, String> {
        Ok(PayoutResult {
            provider: "mock".into(),
            provider_reference: "mock_ready_transfer".into(),
            status: "success".into(),
        })
    }
}

/// Mock provider that simulates Paystack being unfunded and not ready
struct UnreadyProvider;

#[async_trait]
impl PaymentProvider for UnreadyProvider {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
        Ok(PayoutReadiness {
            is_ready: false,
            available_balance: Some(0),
            message: "Paystack account balance is zero or negative; transfers cannot be funded".into(),
        })
    }

    async fn create_payout(&self, _req: &PayoutRequest) -> Result<PayoutResult, String> {
        Err("Your balance is not enough to fulfil this request".into())
    }
}

/// Mock provider that simulates a readiness check failure (e.g., network error)
struct CheckFailureProvider;

#[async_trait]
impl PaymentProvider for CheckFailureProvider {
    async fn check_payout_readiness(&self) -> Result<PayoutReadiness, String> {
        // Simulate a network error or auth issue that prevents checking readiness
        Err("Authentication failed".into())
    }

    async fn create_payout(&self, _req: &PayoutRequest) -> Result<PayoutResult, String> {
        Ok(PayoutResult {
            provider: "mock".into(),
            provider_reference: "unused".into(),
            status: "unused".into(),
        })
    }
}

#[tokio::test]
async fn withdrawal_succeeds_when_payout_ready() {
    let mut state = state().await;
    state.payment_provider = Arc::new(ReadyProvider);
    let app = aframp::router(state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "payout_ready").await;

    sqlx::query(
        "INSERT INTO balances (merchant_id, asset, available, pending)
         VALUES ($1::uuid, 'cNGN', 5_000_000, 0)
         ON CONFLICT (merchant_id, asset) DO UPDATE SET available = 5_000_000, pending = 0",
    )
    .bind(&merchant_id)
    .execute(&state.db)
    .await
    .unwrap();

    let (status, json) = send(
        app.clone(),
        "POST",
        "/withdraw",
        Some(&token),
        Some(json!({
            "amount_stroops": 2_000_000,
            "asset": "cNGN",
            "bank_code": "058",
            "account_number": "0123456789"
        })),
    )
    .await;
    assert_eq!(
        status, StatusCode::OK,
        "withdrawal should succeed when payout is ready: {json}"
    );
    assert_eq!(json["status"], "pending");
    assert_eq!(json["amount_stroops"], 2_000_000);

    // Verify balance was debited
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT available FROM balances WHERE merchant_id = $1::uuid AND asset = 'cNGN'",
    )
    .bind(&merchant_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(balance, 3_000_000, "balance should be debited");
}

#[tokio::test]
async fn withdrawal_rejected_when_payout_not_ready() {
    let mut state = state().await;
    state.payment_provider = Arc::new(UnreadyProvider);
    let app = aframp::router(state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "payout_not_ready").await;

    sqlx::query(
        "INSERT INTO balances (merchant_id, asset, available, pending)
         VALUES ($1::uuid, 'cNGN', 5_000_000, 0)
         ON CONFLICT (merchant_id, asset) DO UPDATE SET available = 5_000_000, pending = 0",
    )
    .bind(&merchant_id)
    .execute(&state.db)
    .await
    .unwrap();

    let (status, json) = send(
        app.clone(),
        "POST",
        "/withdraw",
        Some(&token),
        Some(json!({
            "amount_stroops": 2_000_000,
            "asset": "cNGN",
            "bank_code": "058",
            "account_number": "0123456789"
        })),
    )
    .await;
    assert_eq!(
        status, StatusCode::BAD_REQUEST,
        "withdrawal should be rejected when payout is not ready: {json}"
    );
    assert_eq!(json["code"], "PAYOUT_NOT_READY");
    assert!(
        json["error"].as_str().unwrap().contains("balance is zero"),
        "error message should mention zero balance: {}",
        json["error"]
    );

    // Verify balance was NOT debited (this is critical for the acceptance criteria)
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT available FROM balances WHERE merchant_id = $1::uuid AND asset = 'cNGN'",
    )
    .bind(&merchant_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(
        balance, 5_000_000,
        "balance should NOT be debited when payout is not ready"
    );

    // Verify no withdrawal record was created (balance was never debited)
    let (status, json) = send(app.clone(), "GET", "/withdrawals", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let withdrawals = json.as_array().unwrap();
    assert_eq!(
        withdrawals.len(),
        0,
        "no withdrawal record should exist when request was rejected for unavailable funding"
    );
}

#[tokio::test]
async fn withdrawal_rejected_when_readiness_check_fails() {
    let mut state = state().await;
    state.payment_provider = Arc::new(CheckFailureProvider);
    let app = aframp::router(state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "readiness_check_fail").await;

    sqlx::query(
        "INSERT INTO balances (merchant_id, asset, available, pending)
         VALUES ($1::uuid, 'cNGN', 5_000_000, 0)
         ON CONFLICT (merchant_id, asset) DO UPDATE SET available = 5_000_000, pending = 0",
    )
    .bind(&merchant_id)
    .execute(&state.db)
    .await
    .unwrap();

    let (status, json) = send(
        app.clone(),
        "POST",
        "/withdraw",
        Some(&token),
        Some(json!({
            "amount_stroops": 2_000_000,
            "asset": "cNGN",
            "bank_code": "058",
            "account_number": "0123456789"
        })),
    )
    .await;
    assert_eq!(
        status, StatusCode::BAD_GATEWAY,
        "withdrawal should fail with 502 when readiness check fails: {json}"
    );
    assert_eq!(json["code"], "PAYOUT_FAILED");

    // Verify balance was NOT debited
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT available FROM balances WHERE merchant_id = $1::uuid AND asset = 'cNGN'",
    )
    .bind(&merchant_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(
        balance, 5_000_000,
        "balance should NOT be debited when readiness check fails"
    );

    // Verify no withdrawal record was created
    let (status, json) = send(app.clone(), "GET", "/withdrawals", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let withdrawals = json.as_array().unwrap();
    assert_eq!(
        withdrawals.len(),
        0,
        "no withdrawal record should exist when readiness check fails"
    );
}

#[tokio::test]
async fn withdrawal_balance_preserved_across_ready_and_unready() {
    let mut state = state().await;

    // First, set provider to ready and do a successful withdrawal
    state.payment_provider = Arc::new(ReadyProvider);
    let app = aframp::router(state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "balance_preservation").await;

    sqlx::query(
        "INSERT INTO balances (merchant_id, asset, available, pending)
         VALUES ($1::uuid, 'cNGN', 10_000_000, 0)
         ON CONFLICT (merchant_id, asset) DO UPDATE SET available = 10_000_000, pending = 0",
    )
    .bind(&merchant_id)
    .execute(&state.db)
    .await
    .unwrap();

    let (status, _) = send(
        app.clone(),
        "POST",
        "/withdraw",
        Some(&token),
        Some(json!({
            "amount_stroops": 3_000_000,
            "asset": "cNGN",
            "bank_code": "058",
            "account_number": "0123456789"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT available FROM balances WHERE merchant_id = $1::uuid AND asset = 'cNGN'",
    )
    .bind(&merchant_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(balance, 7_000_000, "balance after successful withdrawal");

    // Now switch to unready provider and attempt another withdrawal
    state.payment_provider = Arc::new(UnreadyProvider);
    let app = aframp::router(state.clone());

    let (status, json) = send(
        app.clone(),
        "POST",
        "/withdraw",
        Some(&token),
        Some(json!({
            "amount_stroops": 2_000_000,
            "asset": "cNGN",
            "bank_code": "058",
            "account_number": "0123456789"
        })),
    )
    .await;
    assert_eq!(
        status, StatusCode::BAD_REQUEST,
        "withdrawal should be rejected: {json}"
    );

    // Balance should remain at 7,000,000 (not debited)
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT available FROM balances WHERE merchant_id = $1::uuid AND asset = 'cNGN'",
    )
    .bind(&merchant_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(
        balance, 7_000_000,
        "balance should remain unchanged when withdrawal is rejected for unavailable funding"
    );

    // Verify only one withdrawal record exists (from the successful one)
    let (status, json) = send(app.clone(), "GET", "/withdrawals", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let withdrawals = json.as_array().unwrap();
    assert_eq!(withdrawals.len(), 1, "only the successful withdrawal should be recorded");
    assert_eq!(withdrawals[0]["status"], "pending");
    assert_eq!(withdrawals[0]["amount_stroops"], 3_000_000);
}
