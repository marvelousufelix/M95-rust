mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{ensure_merchant, send, state};

async fn app() -> axum::Router {
    aframp::router(state().await)
}

#[tokio::test]
async fn protected_routes_require_token() {
    let app = app().await;
    for (method, path) in [
        ("POST", "/wallet/create"),
        ("GET", "/wallet"),
        ("GET", "/balance"),
        ("GET", "/transactions"),
        ("POST", "/withdraw"),
        ("GET", "/withdrawals"),
    ] {
        let (status, json) = send(app.clone(), method, path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "expected 401 for {method} {path}: {json}"
        );
    }
}

#[tokio::test]
async fn create_and_fetch_wallet() {
    let app = app().await;
    let (token, _) = ensure_merchant(&app, "wallet").await;

    let (status, json) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create wallet failed: {json}");
    let address = json["address"].as_str().unwrap().to_string();
    assert!(!address.is_empty());
    assert_eq!(json["network"], "stellar");

    let (status, json) = send(app.clone(), "GET", "/wallet", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "get wallet failed: {json}");
    assert_eq!(json["address"], address);
}

#[tokio::test]
async fn balance_and_transactions_start_empty() {
    let app = app().await;
    let (token, _) = ensure_merchant(&app, "empty").await;

    let (status, json) = send(app.clone(), "GET", "/balance", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "balance failed: {json}");
    assert_eq!(json, json!([]));

    let (status, json) = send(app.clone(), "GET", "/transactions", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "transactions failed: {json}");
    assert_eq!(json, json!([]));
}

#[tokio::test]
async fn wallet_address_is_stable_per_merchant() {
    let app = app().await;
    let (token_a, _) = ensure_merchant(&app, "stable_a").await;
    let (token_b, _) = ensure_merchant(&app, "stable_b").await;

    send(app.clone(), "POST", "/wallet/create", Some(&token_a), Some(json!({}))).await;
    send(app.clone(), "POST", "/wallet/create", Some(&token_b), Some(json!({}))).await;

    let (_, json_a) = send(app.clone(), "GET", "/wallet", Some(&token_a), None).await;
    let (_, json_b) = send(app.clone(), "GET", "/wallet", Some(&token_b), None).await;
    assert_ne!(json_a["address"], json_b["address"]);
}

#[tokio::test]
async fn wallet_creation_is_idempotent() {
    let app = app().await;
    let (token, _) = ensure_merchant(&app, "idempotent").await;

    // Create first wallet
    let (status1, json1) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status1, StatusCode::OK, "first create failed: {json1}");
    let address1 = json1["address"].as_str().unwrap().to_string();
    let id1 = json1["id"].as_str().unwrap().to_string();

    // Retry wallet creation — should return the same wallet
    let (status2, json2) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status2, StatusCode::OK, "retry create failed: {json2}");
    let address2 = json2["address"].as_str().unwrap().to_string();
    let id2 = json2["id"].as_str().unwrap().to_string();

    assert_eq!(
        address1, address2,
        "repeated wallet creation should return the same address (wallet is idempotent)"
    );
    assert_eq!(
        id1, id2,
        "repeated wallet creation should return the same wallet ID"
    );

    // Verify GET /wallet also returns the same wallet
    let (status3, json3) = send(app.clone(), "GET", "/wallet", Some(&token), None).await;
    assert_eq!(status3, StatusCode::OK, "get wallet failed: {json3}");
    assert_eq!(json3["address"], address1);
    assert_eq!(json3["id"], id1);
}

#[tokio::test]
async fn wallet_creation_returns_existing_active_on_retry() {
    let app = app().await;
    let (token, _) = ensure_merchant(&app, "retry_existing").await;

    // Create wallet
    let (status1, json1) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status1, StatusCode::OK, "create failed: {json1}");
    let wallet1_id: uuid::Uuid = json1["id"].as_str().unwrap().parse().unwrap();

    // Simulate multiple concurrent or sequential retries
    for _ in 0..3 {
        let (status, json) = send(
            app.clone(),
            "POST",
            "/wallet/create",
            Some(&token),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "retry failed: {json}");
        let wallet_id: uuid::Uuid = json["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            wallet_id, wallet1_id,
            "all retries should return the same wallet ID"
        );
    }
}

#[tokio::test]
async fn concurrent_wallet_creation_cannot_create_multiple_active() {
    let test_state = state().await;
    let app = aframp::router(test_state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "concurrent").await;

    // Spawn multiple concurrent wallet creation requests
    let mut handles = vec![];
    for _ in 0..5 {
        let app = app.clone();
        let token = token.clone();
        let handle = tokio::spawn(async move {
            let (status, json) = send(
                app,
                "POST",
                "/wallet/create",
                Some(&token),
                Some(json!({})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "create failed: {json}");
            json["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap()
        });
        handles.push(handle);
    }

    // Wait for all tasks and collect wallet IDs
    let mut wallet_ids = vec![];
    for handle in handles {
        let id = handle.await.expect("task panicked");
        wallet_ids.push(id);
    }

    // Verify all concurrent requests returned the same wallet ID
    let first_id = wallet_ids[0];
    for id in wallet_ids.iter() {
        assert_eq!(
            *id, first_id,
            "concurrent creation attempts should return the same wallet"
        );
    }

    // Verify database has only one active wallet for this merchant
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM wallets WHERE merchant_id = $1 AND is_active = true"
    )
    .bind(merchant_id)
    .fetch_one(&test_state.db)
    .await
    .expect("failed to query active wallet count");

    assert_eq!(
        active_count, 1,
        "merchant should have exactly one active wallet after concurrent creation"
    );
}

#[tokio::test]
async fn payment_requests_use_active_wallet_after_retries() {
    let test_state = state().await;
    let app = aframp::router(test_state.clone());
    let (token, _) = ensure_merchant(&app, "pr_active_wallet").await;

    // Create initial wallet
    let (status, wallet1) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create wallet failed: {wallet1}");
    let wallet1_id = wallet1["id"].as_str().unwrap().to_string();
    let wallet1_address = wallet1["address"].as_str().unwrap().to_string();

    // Create a payment request with the first wallet
    let (status, pr1) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create payment request failed: {pr1}");
    assert_eq!(pr1["address"], wallet1_address);

    // Retry wallet creation (idempotent)
    let (status, wallet2) = send(
        app.clone(),
        "POST",
        "/wallet/create",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "retry create wallet failed: {wallet2}");
    let wallet2_id = wallet2["id"].as_str().unwrap().to_string();
    assert_eq!(
        wallet1_id, wallet2_id,
        "retry should return same wallet"
    );

    // Create another payment request after retry
    let (status, pr2) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 20_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create second payment request failed: {pr2}");
    assert_eq!(
        pr2["address"], wallet1_address,
        "payment request should continue using the active wallet after idempotent retry"
    );

    // Verify both payment requests use the same wallet
    let pr1_wallet_id: uuid::Uuid = pr1["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap();
    let pr2_wallet_id: uuid::Uuid = pr2["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap();

    let wallet_for_pr1: uuid::Uuid = sqlx::query_scalar(
        "SELECT wallet_id FROM payment_requests WHERE id = $1"
    )
    .bind(pr1_wallet_id)
    .fetch_one(&test_state.db)
    .await
    .expect("failed to fetch payment request 1");

    let wallet_for_pr2: uuid::Uuid = sqlx::query_scalar(
        "SELECT wallet_id FROM payment_requests WHERE id = $1"
    )
    .bind(pr2_wallet_id)
    .fetch_one(&test_state.db)
    .await
    .expect("failed to fetch payment request 2");

    assert_eq!(
        wallet_for_pr1, wallet_for_pr2,
        "both payment requests should reference the same wallet"
    );
}
