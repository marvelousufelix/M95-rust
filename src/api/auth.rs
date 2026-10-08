use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

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
    let token = jwt::sign(&state.jwt_secret, user.id, Some(merchant.id))
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
            return Err(crate::error::too_many_requests(
                "too many login attempts, please try again later",
                retry_after,
            ));
        }
        Ok(()) => {}
    }

    match users::login(&state.db, &req.email, &req.password).await {
        Ok((user, merchant)) => {
            let token = jwt::sign(&state.jwt_secret, user.id, merchant.as_ref().map(|m| m.id))
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

/// Drops the session cookie. Deliberately unauthenticated: a browser holding an
/// expired or malformed session still needs a way to clear it.
pub async fn logout(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
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
