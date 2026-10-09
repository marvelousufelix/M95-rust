use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::Json;

use crate::auth::{cookie, jwt};
use crate::error::{ApiError, ErrorCode};
use crate::AppState;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: uuid::Uuid,
    pub merchant_id: Option<uuid::Uuid>,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = (StatusCode, Json<ApiError>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // API clients send a bearer token; browsers send the HttpOnly session
        // cookie, which JS on the page cannot read. Either proves the session.
        let token = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| cookie::from_headers(&parts.headers))
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(ApiError {
                        error: "missing session cookie or bearer token".into(),
                        code: Some(ErrorCode::InvalidCredentials.as_str().to_string()),
                        field: None,
                    }),
                )
            })?;
        let claims = jwt::verify(&state.jwt_secret, token)
            .map_err(|_| {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(ApiError {
                        error: "invalid or expired token".into(),
                        code: Some(ErrorCode::InvalidCredentials.as_str().to_string()),
                        field: None,
                    }),
                )
            })?;
        
        // Check if session has been revoked
        let revoked = sqlx::query("SELECT 1 FROM revoked_sessions WHERE session_id = $1")
            .bind(claims.session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                eprintln!("Error checking revocation status: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: "database error".into(),
                        code: Some(ErrorCode::InternalError.as_str().to_string()),
                        field: None,
                    }),
                )
            })?;
        
        if revoked.is_some() {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ApiError {
                    error: "invalid or expired token".into(),
                    code: Some(ErrorCode::InvalidCredentials.as_str().to_string()),
                    field: None,
                }),
            ));
        }
        
        Ok(AuthUser {
            user_id: claims.sub,
            merchant_id: claims.merchant_id,
        })
    }
}
