# Session Revocation Implementation

## Overview

Server-side session revocation ensures that when a user logs out, their JWT token becomes immediately invalid for both cookie-authenticated and bearer-authenticated clients. This prevents unauthorized use of copied or intercepted tokens.

## Architecture

### Database Schema

**Table: `revoked_sessions`**
```sql
CREATE TABLE revoked_sessions (
  session_id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id),
  revoked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_revoked_sessions_expires_at ON revoked_sessions(expires_at);
```

- **session_id**: Unique identifier for the JWT session (UUIDv4)
- **user_id**: User who owns the revoked session
- **revoked_at**: Timestamp when the session was revoked
- **expires_at**: When the entry can be safely deleted (JWT expiry + 24 hours)

The `expires_at` index enables efficient cleanup of expired entries without scanning the whole table.

### JWT Changes

#### Claims Structure

```rust
pub struct Claims {
    pub sub: Uuid,                      // User ID
    pub merchant_id: Option<Uuid>,      // Merchant ID
    pub session_id: Uuid,               // NEW: Session identifier
    pub exp: usize,                     // Expiry timestamp
    pub iat: usize,                     // Issued-at timestamp
}
```

Each JWT now includes a unique `session_id` generated at token creation. This allows the server to identify and revoke specific sessions.

#### Token Signing

```rust
pub fn sign(secret: &str, user_id: Uuid, merchant_id: Option<Uuid>) 
  -> Result<(String, Uuid), jsonwebtoken::errors::Error> {
    let session_id = Uuid::new_v4();
    // ... create Claims with session_id ...
    Ok((token, session_id))  // Returns both token and session_id
}
```

The sign function now returns both the token and its session_id, allowing callers to track sessions if needed.

### Revocation Check in AuthUser Extractor

The `AuthUser` extractor verifies the token and then checks if the session has been revoked:

```rust
impl FromRequestParts<AppState> for AuthUser {
    async fn from_request_parts(...) -> Result<Self, Self::Rejection> {
        // Verify JWT signature and expiry
        let claims = jwt::verify(&state.jwt_secret, token)?;
        
        // Check if session is revoked
        let revoked = sqlx::query(
            "SELECT 1 FROM revoked_sessions WHERE session_id = $1"
        )
        .bind(claims.session_id)
        .fetch_optional(&state.db)
        .await?;
        
        if revoked.is_some() {
            return Err(ApiError { 
                error: "invalid or expired token",
                code: "INVALID_CREDENTIALS" 
            });
        }
        
        Ok(AuthUser { user_id: claims.sub, merchant_id: claims.merchant_id })
    }
}
```

If a matching session_id is found in the revoked_sessions table, the token is rejected with a `401 UNAUTHORIZED` response.

### Logout Endpoint

```rust
pub async fn logout(State(state): State<AppState>, req: Request) 
  -> ApiResult<impl IntoResponse> {
    // Extract Bearer token if present
    if let Some(token) = extract_bearer_token(&req) {
        if let Ok(claims) = jwt::verify(&state.jwt_secret, token) {
            let expires_at = Utc::now() + Duration::hours(jwt::TOKEN_TTL_HOURS);
            
            // Record the revoked session
            sqlx::query(
                "INSERT INTO revoked_sessions (session_id, user_id, expires_at) 
                 VALUES ($1, $2, $3)"
            )
            .bind(claims.session_id)
            .bind(claims.sub)
            .bind(expires_at)
            .execute(&state.db)
            .await?;
        }
    }
    
    // Clear cookie for all clients
    let cookie = state.cookie.clear()?;
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]))
}
```

The logout endpoint:
1. Extracts the Bearer token (if provided)
2. Verifies the token to get the session_id
3. Inserts the session_id into the revoked_sessions table with an expiry time
4. Clears the session cookie (for cookie-authenticated clients)

## Behavior

### For Bearer Token Clients

1. **Pre-logout**: Token works, extractor finds no revoked session entry
2. **Logout call**: Bearer token is extracted, verified, and the session_id is recorded in revoked_sessions
3. **Post-logout**: Same bearer token is rejected because its session_id now exists in revoked_sessions, returning `401 UNAUTHORIZED`

### For Cookie-Authenticated Clients

1. **Pre-logout**: Session cookie authenticates the request normally
2. **Logout call**: Cookie is cleared immediately via `Set-Cookie: Max-Age=0`
3. **Post-logout**: Cookie is absent from subsequent requests; revoked session entry prevents token reuse if cookie was copied elsewhere

### Session Persistence Across Restarts

The revoked session entry is persisted in the database. Even after the server restarts:

1. A revoked token will still be recognized as invalid
2. The database lookup happens on every authenticated request
3. No in-memory state is required

## Cleanup

The `expires_at` column records when the session expires (typically 24 hours from revocation). Background cleanup can remove expired entries:

```sql
DELETE FROM revoked_sessions WHERE expires_at < now();
```

This is a safe, non-urgent cleanup that can run during off-peak hours or as a periodic maintenance task.

## Error Handling

When a revoked session is detected:
- Status: `401 UNAUTHORIZED`
- Response: `{ "error": "invalid or expired token", "code": "INVALID_CREDENTIALS" }`
- Same error as an expired or malformed token (intentionally indistinguishable)

This preserves the existing API contract and makes the client behavior consistent regardless of the reason for invalidity.

## Testing

Tests verify:

1. **Bearer tokens work before logout**: Token successfully authenticates a request
2. **Bearer tokens rejected after logout**: Same token gets `401 UNAUTHORIZED` after logout
3. **Cookie tokens rejected after logout**: Session cookie is cleared and revoked session blocks reuse
4. **Persistence across restarts**: Creating a new app instance still rejects the revoked token
5. **Error codes**: Revoked tokens return the correct `INVALID_CREDENTIALS` error code

All existing authentication tests continue to pass, ensuring backward compatibility.
