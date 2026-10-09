mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{ensure_merchant, send, state};

async fn create_wallet(app: &axum::Router, token: &str) {
    let (status, json) = send(app.clone(), "POST", "/wallet/create", Some(token), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "wallet create failed: {json}");
}

#[tokio::test]
async fn payment_request_requires_wallet() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_no_wallet").await;

    let (status, json) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expected rejection: {json}");
    assert_eq!(json["error"], "create a wallet before generating payment requests");
}

#[tokio::test]
async fn payment_request_create_and_fetch_publicly() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, merchant_id) = ensure_merchant(&app, "pr_create").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    assert_eq!(created["status"], "pending");
    assert_eq!(created["asset"], "XLM", "default asset should be XLM, not cNGN");
    assert_eq!(created["merchant_id"], merchant_id);
    assert!(created["memo"].as_str().unwrap().len() >= 8);
    let sep7 = created["sep7_uri"].as_str().expect("XLM requests should have a sep7_uri");
    assert!(sep7.starts_with("web+stellar:pay?destination="));
    assert!(sep7.contains(&format!("memo={}", created["memo"].as_str().unwrap())));

    // Fetch with NO auth token — this must be publicly readable so a
    // customer's wallet app can look it up before paying.
    let id = created["id"].as_str().unwrap();
    let (status, fetched) = send(app.clone(), "GET", &format!("/payment-requests/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "public fetch failed: {fetched}");
    assert_eq!(fetched["id"], created["id"]);
    assert_eq!(fetched["status"], "pending");
}

#[tokio::test]
async fn payment_request_cngn_has_no_sep7_uri_yet() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_cngn").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000, "asset": "cNGN" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    assert_eq!(created["asset"], "cNGN");
    assert!(
        created["sep7_uri"].is_null(),
        "cNGN has no configured issuer address yet, so no QR should be generated"
    );
}

#[tokio::test]
async fn payment_request_reports_expired_past_its_expiry() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_expiry").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 5_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    let id = created["id"].as_str().unwrap();

    // Force it into the past directly — no need to actually wait.
    sqlx::query("UPDATE payment_requests SET expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();

    let (status, fetched) = send(app.clone(), "GET", &format!("/payment-requests/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["status"], "expired", "past-expiry pending row should report as expired");
}

#[tokio::test]
async fn payment_request_list_is_scoped_to_the_authenticated_merchant() {
    let state = state().await;
    let app = aframp::router(state.clone());

    let (token_a, _) = ensure_merchant(&app, "pr_list_a").await;
    create_wallet(&app, &token_a).await;
    let (token_b, _) = ensure_merchant(&app, "pr_list_b").await;
    create_wallet(&app, &token_b).await;

    for amount in [10_000_000, 20_000_000] {
        let (status, json) = send(
            app.clone(),
            "POST",
            "/payment-requests",
            Some(&token_a),
            Some(json!({ "amount_stroops": amount })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create failed: {json}");
    }
    let (status, json) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token_b),
        Some(json!({ "amount_stroops": 99_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {json}");

    let (status, list_a) = send(app.clone(), "GET", "/payment-requests", Some(&token_a), None).await;
    assert_eq!(status, StatusCode::OK, "list failed: {list_a}");
    let rows = list_a.as_array().unwrap();
    assert_eq!(rows.len(), 2, "merchant A should see only their own two requests");
    // Newest first.
    assert_eq!(rows[0]["amount_stroops"], 20_000_000);
    assert_eq!(rows[1]["amount_stroops"], 10_000_000);
    assert!(
        rows.iter().all(|r| r["sep7_uri"].is_string()),
        "listed XLM requests should each carry a scannable URI"
    );

    let (status, list_b) = send(app.clone(), "GET", "/payment-requests", Some(&token_b), None).await;
    assert_eq!(status, StatusCode::OK);
    let rows_b = list_b.as_array().unwrap();
    assert_eq!(rows_b.len(), 1, "merchant B must not see merchant A's requests");
    assert_eq!(rows_b[0]["amount_stroops"], 99_000_000);
}

#[tokio::test]
async fn payment_request_list_requires_auth() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (status, _) = send(app.clone(), "GET", "/payment-requests", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn payment_request_marked_paid_on_memo_correlated_deposit() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_paid").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 25_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    let id: uuid::Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let memo = created["memo"].as_str().unwrap();

    // Simulates exactly what blockchain::worker::process_deposit does once it
    // detects a deposit whose memo matches a pending request — without needing
    // a real signed Stellar transaction (that's a separate, unbuilt capability;
    // see PRD's "Stellar transaction creation" row).
    let wallet_id: uuid::Uuid =
        sqlx::query_scalar("SELECT wallet_id FROM payment_requests WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    let pending = aframp::services::payment_requests::find_pending_by_wallet_and_memo(&state.db, wallet_id, memo)
        .await
        .unwrap();
    assert!(pending.is_some(), "should find the pending request by wallet_id + memo");

    let fake_payment_id = uuid::Uuid::new_v4();
    // A real payments row is required by the FK — insert one directly, standing
    // in for what payments::record_deposit would have created from a real
    // detected deposit.
    sqlx::query(
        "INSERT INTO payments (id, merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status)
         VALUES ($1, $2, $3, 'PLACEHOLDER', $4, 25000000, 'XLM', 'stellar', 'confirmed')",
    )
    .bind(fake_payment_id)
    .bind(created["merchant_id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap())
    .bind(wallet_id)
    .bind(format!("test_tx_{memo}"))
    .execute(&state.db)
    .await
    .unwrap();

    aframp::services::payment_requests::mark_paid(&state.db, id, fake_payment_id)
        .await
        .unwrap();

    let (status, fetched) = send(app.clone(), "GET", &format!("/payment-requests/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["status"], "paid");
}


#[tokio::test]
async fn payment_request_xlm_has_sep7_uri_regardless_of_cngn_issuer() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_xlm_always_uri").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000, "asset": "XLM" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    assert_eq!(created["asset"], "XLM");
    let sep7 = created["sep7_uri"].as_str().expect("XLM requests must have a sep7_uri");
    assert!(sep7.starts_with("web+stellar:pay?destination="));
    // XLM URIs should NOT include asset_code or asset_issuer (native asset)
    assert!(!sep7.contains("asset_code="));
    assert!(!sep7.contains("asset_issuer="));
}

#[tokio::test]
async fn payment_request_cngn_with_issuer_has_sep7_uri() {
    // This test uses the actual cngn_issuer from state.config if set.
    // If CNGN_ISSUER_ADDRESS is not set in the test environment,
    // we still test that when it IS set, cNGN URIs are generated correctly.
    let state = state().await;

    // Only run this test if cNGN_ISSUER_ADDRESS is configured.
    if state.config.cngn_issuer.is_none() {
        // Skip — issuer not configured, can't test with a real issuer.
        eprintln!("Skipping test: CNGN_ISSUER_ADDRESS not configured in environment");
        return;
    }

    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_cngn_with_issuer").await;
    create_wallet(&app, &token).await;

    let (status, created) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000, "asset": "cNGN" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    assert_eq!(created["asset"], "cNGN");

    let sep7 = created["sep7_uri"]
        .as_str()
        .expect("cNGN with issuer configured should have a sep7_uri");
    assert!(sep7.starts_with("web+stellar:pay?destination="));
    assert!(sep7.contains("asset_code=cNGN"));
    assert!(sep7.contains(&format!("asset_issuer={}", state.config.cngn_issuer.as_ref().unwrap())));
    let memo = created["memo"].as_str().unwrap();
    assert!(sep7.contains(&format!("memo={memo}")));
}

#[tokio::test]
async fn payment_request_list_reflects_cngn_issuer_configuration() {
    let state = state().await;
    let app = aframp::router(state.clone());
    let (token, _) = ensure_merchant(&app, "pr_list_cngn").await;
    create_wallet(&app, &token).await;

    // Create both XLM and cNGN requests.
    let (status, xlm_req) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000, "asset": "XLM" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "XLM create failed: {xlm_req}");

    let (status, cngn_req) = send(
        app.clone(),
        "POST",
        "/payment-requests",
        Some(&token),
        Some(json!({ "amount_stroops": 10_000_000, "asset": "cNGN" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cNGN create failed: {cngn_req}");

    let (status, list) = send(app.clone(), "GET", "/payment-requests", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "list failed: {list}");
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 2, "should have both requests");

    // XLM should have a sep7_uri
    let xlm_row = rows.iter().find(|r| r["asset"] == "XLM").expect("should find XLM request");
    assert!(xlm_row["sep7_uri"].is_string(), "XLM should have sep7_uri");
    assert!(xlm_row["sep7_uri"].as_str().unwrap().starts_with("web+stellar:pay?"));

    // cNGN behavior depends on issuer configuration
    let cngn_row = rows.iter().find(|r| r["asset"] == "cNGN").expect("should find cNGN request");
    if state.config.cngn_issuer.is_some() {
        assert!(
            cngn_row["sep7_uri"].is_string(),
            "cNGN should have sep7_uri when issuer is configured"
        );
        assert!(cngn_row["sep7_uri"].as_str().unwrap().contains("asset_code=cNGN"));
    } else {
        assert!(
            cngn_row["sep7_uri"].is_null(),
            "cNGN should have null sep7_uri when issuer is not configured"
        );
    }
}
