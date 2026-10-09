mod common;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use common::{ensure_merchant, send, state};

async fn create_wallet(app: &axum::Router, token: &str) -> String {
    let (status, json) = send(app.clone(), "POST", "/wallet/create", Some(token), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "wallet create failed: {json}");
    json["address"]
        .as_str()
        .expect("wallet response should have address")
        .to_string()
}

/// Helper to fetch a payment by ID from the database.
async fn get_payment_by_tx_hash(db: &PgPool, tx_hash: &str) -> Option<(Uuid, String, i64, Option<i64>)> {
    sqlx::query_as::<_, (Uuid, String, i64, Option<i64>)>(
        "SELECT id, status, amount_stroops, confirmation_ledger FROM payments WHERE tx_hash = $1",
    )
    .bind(tx_hash)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
}

/// Helper to fetch a merchant's balance.
async fn get_balance(db: &PgPool, merchant_id: Uuid, asset: &str) -> (i64, i64) {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT available, pending FROM balances WHERE merchant_id = $1 AND asset = $2",
    )
    .bind(merchant_id)
    .bind(asset)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
    .unwrap_or((0, 0))
}

#[tokio::test]
async fn deposit_starts_in_detected_status() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_detected").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Simulate a deposit record by directly inserting into the database
    let tx_hash = "test_tx_detected_001";
    sqlx::query(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold)
         SELECT $1, id, address, $2, 10000000, 'XLM', 'stellar', 'detected', 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .execute(db)
    .await
    .unwrap();

    let (_, status, _, confirmation_ledger) = get_payment_by_tx_hash(db, tx_hash).await.unwrap();
    assert_eq!(status, "detected");
    assert_eq!(confirmation_ledger, None, "confirmation_ledger should be None for detected payments");
}

#[tokio::test]
async fn deposit_transitions_detected_to_verified_with_confirmation_ledger() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_verified").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create a detected payment
    let tx_hash = "test_tx_verified_001";
    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold)
         SELECT $1, id, address, $2, 10000000, 'XLM', 'stellar', 'detected', 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .fetch_one(db)
    .await
    .unwrap();

    // Transition to verified
    aframp::services::payments::set_status(db, payment_id, aframp::models::UpdatePaymentStatus::Verified)
        .await
        .unwrap();

    // Set confirmation ledger
    let confirmation_ledger = 47118521i64;
    aframp::services::payments::set_confirmation_ledger(db, payment_id, confirmation_ledger)
        .await
        .unwrap();

    let (_, status, _, stored_ledger) = get_payment_by_tx_hash(db, tx_hash).await.unwrap();
    assert_eq!(status, "verified");
    assert_eq!(stored_ledger, Some(confirmation_ledger));
}

#[tokio::test]
async fn balance_added_to_pending_on_verified() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_pending_balance").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create and verify a payment
    let tx_hash = "test_tx_pending_balance_001";
    let amount_stroops = 50_000_000i64;
    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold)
         SELECT $1, id, address, $2, $3, 'XLM', 'stellar', 'detected', 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .bind(amount_stroops)
    .fetch_one(db)
    .await
    .unwrap();

    // Transition to verified and add to pending balance
    aframp::services::payments::set_status(db, payment_id, aframp::models::UpdatePaymentStatus::Verified)
        .await
        .unwrap();
    aframp::services::payments::set_confirmation_ledger(db, payment_id, 47118521)
        .await
        .unwrap();

    aframp::services::balances::apply_delta(
        db,
        &aframp::models::UpdateBalance {
            merchant_id,
            asset: "XLM".to_string(),
            available_delta: 0,
            pending_delta: amount_stroops,
        },
    )
    .await
    .unwrap();

    let (available, pending) = get_balance(db, merchant_id, "XLM").await;
    assert_eq!(available, 0, "available balance should be 0 for verified payment");
    assert_eq!(pending, amount_stroops, "pending balance should match the payment amount");
}

#[tokio::test]
async fn deposit_with_insufficient_confirmations_stays_verified() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_insufficient_confirm").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create a verified payment with confirmation_ledger set
    let tx_hash = "test_tx_insufficient_001";
    let confirmation_ledger = 47118521i64;
    let current_ledger = confirmation_ledger + 20; // Only 20 ledgers have passed, need 32
    let amount_stroops = 50_000_000i64;

    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, $3, 'XLM', 'stellar', 'verified', $4, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .bind(amount_stroops)
    .bind(confirmation_ledger)
    .fetch_one(db)
    .await
    .unwrap();

    // Check that it's NOT ready to confirm
    let ready_ids = aframp::services::payments::ready_to_confirm(db, current_ledger)
        .await
        .unwrap();
    assert!(
        !ready_ids.contains(&payment_id),
        "payment should NOT be ready to confirm with only 20 ledgers passed"
    );

    // Verify it's still in "verified" status
    let (_, status, _, _) = get_payment_by_tx_hash(db, tx_hash).await.unwrap();
    assert_eq!(status, "verified");
}

#[tokio::test]
async fn deposit_with_threshold_reached_moves_to_confirmed() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_threshold_reached").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create a verified payment with confirmation_ledger set
    let tx_hash = "test_tx_threshold_001";
    let confirmation_ledger = 47118521i64;
    let current_ledger = confirmation_ledger + 32; // Exactly 32 ledgers have passed
    let amount_stroops = 50_000_000i64;

    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, $3, 'XLM', 'stellar', 'verified', $4, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .bind(amount_stroops)
    .bind(confirmation_ledger)
    .fetch_one(db)
    .await
    .unwrap();

    // Check that it IS ready to confirm
    let ready_ids = aframp::services::payments::ready_to_confirm(db, current_ledger)
        .await
        .unwrap();
    assert!(
        ready_ids.contains(&payment_id),
        "payment should be ready to confirm after 32 ledgers"
    );
}

#[tokio::test]
async fn balance_moves_from_pending_to_available_on_confirmed() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_balance_move").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create a verified payment and set initial balance to pending
    let tx_hash = "test_tx_balance_move_001";
    let confirmation_ledger = 47118521i64;
    let amount_stroops = 50_000_000i64;

    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, $3, 'XLM', 'stellar', 'verified', $4, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .bind(amount_stroops)
    .bind(confirmation_ledger)
    .fetch_one(db)
    .await
    .unwrap();

    // Add to pending balance
    aframp::services::balances::apply_delta(
        db,
        &aframp::models::UpdateBalance {
            merchant_id,
            asset: "XLM".to_string(),
            available_delta: 0,
            pending_delta: amount_stroops,
        },
    )
    .await
    .unwrap();

    let (available_before, pending_before) = get_balance(db, merchant_id, "XLM").await;
    assert_eq!(available_before, 0);
    assert_eq!(pending_before, amount_stroops);

    // Transition to confirmed
    aframp::services::payments::set_status(db, payment_id, aframp::models::UpdatePaymentStatus::Confirmed)
        .await
        .unwrap();

    // Move balance from pending to available
    aframp::services::balances::apply_delta(
        db,
        &aframp::models::UpdateBalance {
            merchant_id,
            asset: "XLM".to_string(),
            available_delta: amount_stroops,
            pending_delta: -amount_stroops,
        },
    )
    .await
    .unwrap();

    let (available_after, pending_after) = get_balance(db, merchant_id, "XLM").await;
    assert_eq!(available_after, amount_stroops, "available balance should have the full amount");
    assert_eq!(pending_after, 0, "pending balance should be zero after confirmation");
}

#[tokio::test]
async fn multiple_payments_at_different_confirmation_stages() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_multiple_stages").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    let current_ledger = 47118600i64;

    // Payment 1: detected (no ledger)
    let tx1: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold)
         SELECT $1, id, address, $2, 10000000, 'XLM', 'stellar', 'detected', 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind("tx_multiple_detected")
    .fetch_one(db)
    .await
    .unwrap();

    // Payment 2: verified, insufficient confirmations (only 10 ledgers passed)
    let ledger2 = current_ledger - 10;
    let tx2: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, 20000000, 'XLM', 'stellar', 'verified', $3, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind("tx_multiple_verified_10")
    .bind(ledger2)
    .fetch_one(db)
    .await
    .unwrap();

    // Payment 3: verified, exactly at threshold (32 ledgers passed)
    let ledger3 = current_ledger - 32;
    let tx3: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, 30000000, 'XLM', 'stellar', 'verified', $3, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind("tx_multiple_verified_32")
    .bind(ledger3)
    .fetch_one(db)
    .await
    .unwrap();

    // Payment 4: verified, beyond threshold (40 ledgers passed)
    let ledger4 = current_ledger - 40;
    let tx4: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_ledger, confirmation_threshold)
         SELECT $1, id, address, $2, 40000000, 'XLM', 'stellar', 'verified', $3, 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1
         RETURNING id",
    )
    .bind(merchant_id)
    .bind("tx_multiple_verified_40")
    .bind(ledger4)
    .fetch_one(db)
    .await
    .unwrap();

    // Query for ready-to-confirm payments
    let ready_ids = aframp::services::payments::ready_to_confirm(db, current_ledger)
        .await
        .unwrap();

    assert!(!ready_ids.contains(&tx1), "detected payment should not be ready");
    assert!(!ready_ids.contains(&tx2), "payment with 10 confirmations should not be ready");
    assert!(
        ready_ids.contains(&tx3),
        "payment with exactly 32 confirmations should be ready"
    );
    assert!(
        ready_ids.contains(&tx4),
        "payment with 40 confirmations should be ready"
    );
    assert_eq!(
        ready_ids.len(),
        2,
        "should have exactly 2 payments ready to confirm"
    );
}

#[tokio::test]
async fn balance_unchanged_when_payment_detected() {
    let Some(state) = state().await else {
        return;
    };
    let db = &state.db;
    let app = aframp::router(state.clone());
    let (token, merchant_id_str) = ensure_merchant(&app, "deposit_no_balance_on_detect").await;
        let merchant_id: Uuid = merchant_id_str.parse().expect("invalid merchant_id uuid");
    let _wallet_address = create_wallet(&app, &token).await;

    // Create a detected payment
    let tx_hash = "test_tx_no_balance_detect";
    sqlx::query(
        "INSERT INTO payments (merchant_id, wallet_id, wallet_address, tx_hash, amount_stroops, asset, network, status, confirmation_threshold)
         SELECT $1, id, address, $2, 10000000, 'XLM', 'stellar', 'detected', 32
         FROM wallets WHERE merchant_id = $1 LIMIT 1",
    )
    .bind(merchant_id)
    .bind(tx_hash)
    .execute(db)
    .await
    .unwrap();

    // Check that no balance record exists
    let (available, pending) = get_balance(db, merchant_id, "XLM").await;
    assert_eq!(available, 0, "available balance should be 0 for detected payment");
    assert_eq!(pending, 0, "pending balance should be 0 for detected payment");
}
