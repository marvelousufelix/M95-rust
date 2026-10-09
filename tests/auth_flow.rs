mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{send, send_with_cookie, state};

async fn app() -> Option<axum::Router> {
    state().await.map(aframp::router)
}

#[tokio::test]
async fn signup_and_login_success() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("alice+{}@example.com", uuid::Uuid::new_v4().simple());

    let (status, json) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Alice" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {json}");
    assert!(json["token"].as_str().unwrap().len() > 10);
    assert!(json["merchant_id"].as_str().is_some());

    let (status, json) = send(
        app.clone(),
        "POST",
        "/login",
        None,
        Some(json!({ "email": email, "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {json}");
    assert!(json["token"].as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn signup_duplicate_email_conflicts() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("dup+{}@example.com", uuid::Uuid::new_v4().simple());

    send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Dup" })),
    )
    .await;

    let (status, json) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Dup" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "expected conflict: {json}");
    assert_eq!(json["error"], "email already registered");
}

#[tokio::test]
async fn signup_weak_password_rejected() {
    let Some(app) = app().await else {
        return;
    };
    let (status, _) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": "weak@example.com", "password": "short", "name": "Weak" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_wrong_password_unauthorized() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("wrongpw+{}@example.com", uuid::Uuid::new_v4().simple());
    send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Bob" })),
    )
    .await;

    let (status, _) = send(
        app.clone(),
        "POST",
        "/login",
        None,
        Some(json!({ "email": email, "password": "not-the-password" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_returns_profile_for_a_valid_token() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("me+{}@example.com", uuid::Uuid::new_v4().simple());

    let (status, signup) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Me Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");
    let token = signup["token"].as_str().unwrap();

    let (status, me) = send(app.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "me failed: {me}");
    assert_eq!(me["email"], email);
    assert_eq!(me["name"], "Me Tester");
    assert_eq!(me["user_id"], signup["user_id"]);
    assert_eq!(me["merchant_id"], signup["merchant_id"]);
    assert_eq!(me["merchant_name"], "Me Tester");
    assert!(
        me.get("password_hash").is_none(),
        "the password hash must never be serialized to a client"
    );
}

#[tokio::test]
async fn login_sets_an_http_only_session_cookie_that_authenticates() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("cookie+{}@example.com", uuid::Uuid::new_v4().simple());

    let (status, signup, cookies) = send_with_cookie(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Cookie Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");

    let session = cookies
        .iter()
        .find(|c| c.starts_with("aframp_session="))
        .expect("signup must set a session cookie");
    assert!(session.contains("HttpOnly"), "session must be unreadable from JS: {session}");
    assert!(session.contains("Secure"), "session must not travel over plain HTTP: {session}");
    assert!(session.contains("SameSite=Lax"), "session must not ride cross-site requests: {session}");

    // The cookie alone authenticates: no Authorization header in sight.
    let jar = format!("aframp_session={}", signup["token"].as_str().unwrap());
    let (status, me, _) = send_with_cookie(app.clone(), "GET", "/me", Some(&jar), None).await;
    assert_eq!(status, StatusCode::OK, "cookie auth failed: {me}");
    assert_eq!(me["email"], email);
}

#[tokio::test]
async fn logout_clears_the_session_cookie() {
    let Some(app) = app().await else {
        return;
    };
    let (status, _, cookies) =
        send_with_cookie(app.clone(), "POST", "/logout", None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let cleared = cookies
        .iter()
        .find(|c| c.starts_with("aframp_session="))
        .expect("logout must clear the session cookie");
    assert!(cleared.contains("Max-Age=0"), "cookie must expire immediately: {cleared}");
}

#[tokio::test]
async fn a_garbage_session_cookie_is_rejected() {
    let Some(app) = app().await else {
        return;
    };
    let (status, _, _) = send_with_cookie(
        app.clone(),
        "GET",
        "/me",
        Some("aframp_session=not-a-real-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_requires_a_valid_token() {
    let Some(app) = app().await else {
        return;
    };
    let (status, _) = send(app.clone(), "GET", "/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(app.clone(), "GET", "/me", Some("not-a-real-token"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bearer_token_works_before_logout() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("bearer+{}@example.com", uuid::Uuid::new_v4().simple());

    let (status, signup) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Bearer Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");
    let token = signup["token"].as_str().unwrap();

    // Token works before logout
    let (status, me) = send(app.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "token should work before logout: {me}");
    assert_eq!(me["email"], email);
}

#[tokio::test]
async fn bearer_token_rejected_after_logout() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("logout+{}@example.com", uuid::Uuid::new_v4().simple());

    // Sign up and get a token
    let (status, signup) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Logout Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");
    let token = signup["token"].as_str().unwrap();

    // Verify token works before logout
    let (status, _) = send(app.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "token should work before logout");

    // Call logout with the Bearer token
    let (status, _) = send(app.clone(), "POST", "/logout", Some(token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "logout should succeed");

    // Token should now be rejected
    let (status, me_resp) = send(app.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "token should be rejected after logout: {me_resp}"
    );
    assert_eq!(me_resp["error"], "invalid or expired token");
}

#[tokio::test]
async fn cookie_token_rejected_after_logout() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("cookie_logout+{}@example.com", uuid::Uuid::new_v4().simple());

    // Sign up and get a session cookie
    let (status, signup, _) = send_with_cookie(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Cookie Logout Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");

    let token = signup["token"].as_str().unwrap();
    let jar = format!("aframp_session={}", token);

    // Verify token works with cookie before logout
    let (status, _, _) = send_with_cookie(app.clone(), "GET", "/me", Some(&jar), None).await;
    assert_eq!(status, StatusCode::OK, "cookie auth should work before logout");

    // Logout with the cookie
    let (status, _, _) =
        send_with_cookie(app.clone(), "POST", "/logout", Some(&jar), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "logout should succeed");

    // Cookie should now be rejected
    let (status, me_resp, _) =
        send_with_cookie(app.clone(), "GET", "/me", Some(&jar), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "cookie auth should be rejected after logout: {me_resp}"
    );
}

#[tokio::test]
async fn session_revocation_survives_app_restart() {
    let Some(db_state) = state().await else {
        return;
    };
    let email = format!("restart+{}@example.com", uuid::Uuid::new_v4().simple());

    // Create an app instance
    let app1 = aframp::router(db_state.clone());

    // Sign up and get a token
    let (status, signup) = send(
        app1.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Restart Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {signup}");
    let token = signup["token"].as_str().unwrap();

    // Verify token works
    let (status, _) = send(app1.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "token should work initially");

    // Logout with the Bearer token
    let (status, _) = send(app1.clone(), "POST", "/logout", Some(token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "logout should succeed");

    // Create a new app instance (simulating restart)
    let app2 = aframp::router(db_state.clone());

    // Token should still be rejected on new instance (persisted in DB)
    let (status, me_resp) = send(app2.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "token should be rejected on new instance: {me_resp}"
    );
    assert_eq!(me_resp["error"], "invalid or expired token");
}

#[tokio::test]
async fn session_revocation_with_error_code() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("error_code+{}@example.com", uuid::Uuid::new_v4().simple());

    let (status, signup) = send(
        app.clone(),
        "POST",
        "/signup",
        None,
        Some(json!({ "email": email, "password": "password123", "name": "Error Code Tester" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = signup["token"].as_str().unwrap();

    // Logout
    let (status, _) = send(app.clone(), "POST", "/logout", Some(token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Check that revoked token returns proper error code
    let (status, me_resp) = send(app.clone(), "GET", "/me", Some(token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(me_resp["code"], "INVALID_CREDENTIALS");
}
