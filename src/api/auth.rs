use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{Duration, Utc};

use crate::auth::jwt;
use crate::error::{bad_request_field, internal, ApiResult, ErrorCode};
use crate::models::{AuthResponse, LoginRequest, SignupRequest};
use crate::services::users::{self, UserError};
use crate::validation::{is_valid_email, validate_name};
use crate::AppState;

pub async fn signup(
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> ApiResult<impl IntoResponse> {
    if !is_valid_email(&req.email) {
        return Err(bad_request_field("email", "must be a valid email address"));
    }
    if req.password.len() < 8 {
        return Err(bad_request_field(
            "password",
            "must be at least 8 characters",
        ));
    }
    let name = validate_name(&req.name).map_err(|msg| bad_request_field("name", msg))?;
    let (user, merchant) = users::signup(&state.db, &req.email, &req.password, &name)
        .await
        .map_err(map_user_error)?;
    let (token, _session_id) = jwt::sign(&state.jwt_secret, user.id, Some(merchant.id))
        .map_err(internal)?;
    authenticated(
        &state,
        AuthResponse {
            token,
            user_id: user.id,
            merchant_id: Some(merchant.id),
        },
    )
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    if !is_valid_email(&req.email) {
        return Err(bad_request_field("email", "must be a valid email address"));
    }

    // Check rate limit before attempting login
    match state.login_rate_limiter.check_and_record(
        &req.email,
        state.config.login_rate_limit.max_attempts,
        state.config.login_rate_limit.window_secs,
    ) {
        Err(retry_after) => {
            let error_response = Json(crate::error::ApiError {
                error: "too many login attempts, please try again later".into(),
                code: Some(crate::error::ErrorCode::TooManyRequests.as_str().to_string()),
                field: None,
            });
            let headers = [(
                axum::http::header::RETRY_AFTER,
                retry_after.to_string(),
            )];
            return Err((StatusCode::TOO_MANY_REQUESTS, Json(crate::error::ApiError {
                error: "too many login attempts, please try again later".into(),
                code: Some(crate::error::ErrorCode::TooManyRequests.as_str().to_string()),
                field: None,
            })));
        }
        Ok(()) => {}
    }

    match users::login(&state.db, &req.email, &req.password).await {
        Ok((user, merchant)) => {
            let (token, _session_id) = jwt::sign(&state.jwt_secret, user.id, merchant.as_ref().map(|m| m.id))
                .map_err(internal)?;
            // Clear rate limit on successful login
            state.login_rate_limiter.reset(&req.email);
            authenticated(
                &state,
                AuthResponse {
                    token,
                    user_id: user.id,
                    merchant_id: merchant.map(|m| m.id),
                },
            )
        }
        Err(err) => Err(map_user_error(err)),
    }
}

/// Revokes the current session and clears the session cookie.
///
/// Extracts the session token from either the `Authorization: Bearer <token>`
/// header (API clients) or the `aframp_session` cookie (browser clients) and
/// records the `session_id` in `revoked_sessions`. Both paths are covered so
/// a copied bearer token is rejected immediately after the owner logs out.
pub async fn logout(State(state): State<AppState>, req: axum::extract::Request) -> ApiResult<impl IntoResponse> {
    // Resolve the raw token from whichever source the caller used.
    let token: Option<&str> = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| crate::auth::cookie::from_headers(req.headers()));

    if let Some(token) = token {
        if let Ok(claims) = jwt::verify(&state.jwt_secret, token) {
            let expires_at = Utc::now() + Duration::hours(jwt::TOKEN_TTL_HOURS);

            // INSERT … ON CONFLICT DO NOTHING: idempotent — double-logout is fine.
            if let Err(e) = sqlx::query(
                "INSERT INTO revoked_sessions (session_id, user_id, expires_at) \
                 VALUES ($1, $2, $3) ON CONFLICT (session_id) DO NOTHING",
            )
            .bind(claims.session_id)
            .bind(claims.sub)
            .bind(expires_at)
            .execute(&state.db)
            .await
            {
                eprintln!("Error revoking session: {}", e);
                return Err(internal("failed to revoke session"));
            }
        }
    }

    // Clear the session cookie for browser clients regardless of auth source.
    let cookie = state.cookie.clear().map_err(internal)?;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]))
}

/// Sets the session cookie for browsers and echoes the token for API clients.
fn authenticated(state: &AppState, body: AuthResponse) -> ApiResult<impl IntoResponse> {
    let cookie = state.cookie.session(&body.token).map_err(internal)?;
    Ok(([(header::SET_COOKIE, cookie)], Json(body)))
}

fn map_user_error(err: UserError) -> (axum::http::StatusCode, Json<crate::error::ApiError>) {
    match err {
        UserError::EmailTaken => crate::error::conflict(ErrorCode::EmailTaken, "email already registered"),
        UserError::InvalidCredentials => {
            crate::error::unauthorized(ErrorCode::InvalidCredentials, "invalid email or password")
        }
        _ => internal(err),
    }
}
