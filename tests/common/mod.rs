use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use aframp::{AppState, LoginRateLimitConfig, SettlementConfig, IssuerConfig};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

static MIGRATION_LOCK: Mutex<()> = Mutex::new(());
static MIGRATED: AtomicBool = AtomicBool::new(false);

pub async fn state() -> AppState {
    let url = std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "\n\nMissing TEST_DATABASE_URL.\n\
             Integration tests require a running PostgreSQL instance.\n\
             Start one and re-run with:\n\n  \
             TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/m95_test cargo test\n\n\
             See WORKFLOW.md §Testing for full setup instructions.\n"
        )
    });
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "\n\nCould not connect to TEST_DATABASE_URL ({url}):\n  {err}\n\n\
                 Ensure Postgres is running and the database exists.\n\
                 See WORKFLOW.md §Testing for setup instructions.\n"
            )
        });

    let _guard = MIGRATION_LOCK.lock().unwrap();
    if !MIGRATED.swap(true, Ordering::SeqCst) {
        sqlx::migrate!()
            .run(&db)
            .await
            .expect("migrations failed");
    }
    drop(_guard);

    // Create a minimal AppConfig for testing. Most values don't matter; 
    // we just need config.cngn_issuer to test the feature.
    let config = aframp::AppConfig {
        database_url: url.clone(),
        bind_addr: "127.0.0.1:3000".to_string(),
        jwt_secret: aframp::SecretString::new("integration-test-secret".to_string()),
        webhook_secret: aframp::SecretString::new("integration-test-webhook".to_string()),
        stellar_system_wallet: std::sync::Arc::new("G".to_string() + &"X".repeat(55)),
        stellar_horizon_url: "https://horizon-testnet.stellar.org".to_string(),
        stellar_poll_interval_secs: 60,
        wallet_encryption_key: aframp::SecretString::new(
            "0707070707070707070707070707070707070707070707070707070707070707".to_string()
        ),
        paystack_secret_key: aframp::SecretString::new("sk_test_placeholder".to_string()),
        cors_allowed_origins: vec!["http://localhost:3001".to_string()],
        cookie: aframp::CookieConfig {
            secure: true,
            same_site: aframp::SameSite::Lax,
        },
        cngn_issuer: None,
        login_rate_limit: LoginRateLimitConfig {
            max_attempts: 5,
            window_secs: 300,
        },
        settlement: SettlementConfig {
            enabled: false,
            interval_secs: 300,
            window_start_utc: None,
            min_balance_stroops: 1000000,
            max_retries: 3,
            issuer: IssuerConfig {
                api_url: "https://issuer-api.example.com".to_string(),
                api_key: aframp::SecretString::new("test-key".to_string()),
                max_timeout_secs: 30,
            },
        },
    };

    AppState {
        db,
        config,
        jwt_secret: aframp::SecretString::new("integration-test-secret".to_string()),
        webhook_secret: aframp::SecretString::new("integration-test-webhook".to_string()),
        wallet_encryption_key: std::sync::Arc::new([7u8; 32]),
        payment_provider: std::sync::Arc::new(aframp::payments::mock::MockProvider),
        cookie: aframp::CookieConfig {
            secure: true,
            same_site: aframp::SameSite::Lax,
        },
        login_rate_limiter: std::sync::Arc::new(aframp::services::login_rate_limit::LoginRateLimiter::new()),
    }
}

pub async fn send(
    app: Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .header("content-type", "application/json")
        .body(match body {
            Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
            None => Body::empty(),
        })
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Like [`send`], but authenticates with a `Cookie` header the way a browser
/// does and hands back the response's `Set-Cookie` values.
pub async fn send_with_cookie(
    app: Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Vec<String>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let request = builder
        .header("content-type", "application/json")
        .body(match body {
            Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
            None => Body::empty(),
        })
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let set_cookie = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, set_cookie)
}

pub async fn ensure_merchant(app: &Router, seed: &str) -> (String, String) {
    let email = format!("{seed}+{}@example.com", uuid::Uuid::new_v4().simple());
    let (status, json) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(serde_json::json!({
            "email": email,
            "password": "password123",
            "name": "Test Merchant"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {json}");
    (
        json["token"].as_str().unwrap().to_string(),
        json["merchant_id"].as_str().unwrap().to_string(),
    )
}
