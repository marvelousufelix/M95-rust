# Session Revocation Implementation - Summary

## Acceptance Criteria Met

✅ **A token works before logout and is rejected after logout**
- Verified by `bearer_token_works_before_logout` test
- Verified by `bearer_token_rejected_after_logout` test

✅ **Both cookie-authenticated and bearer-authenticated logout paths revoke the session**
- Cookie path tested in `cookie_token_rejected_after_logout`
- Bearer path tested in `bearer_token_rejected_after_logout`

✅ **Expired-token handling and existing authentication error shapes remain consistent**
- Revoked tokens return `401 UNAUTHORIZED` with `{"error":"invalid or expired token","code":"INVALID_CREDENTIALS"}`
- Same error structure as expired or malformed tokens

✅ **Tests verify revocation survives application restart**
- `session_revocation_survives_app_restart` test creates a token, logs out, creates a new app instance, and verifies the token is still rejected
- Revocation is persisted in the database

✅ **Database migration documents storage and cleanup behavior**
- `migrations/0011_session_revocation.sql` creates the `revoked_sessions` table
- Table includes `expires_at` column and index for cleanup queries
- Cleanup can safely remove entries where `expires_at < now()`

## Files Modified

### Core Implementation
- **src/auth/jwt.rs**: Added `session_id` to Claims; sign() returns (token, session_id) tuple
- **src/auth/extractor.rs**: Added revocation check before accepting a token
- **src/api/auth.rs**: Updated logout() to extract Bearer token and insert into revoked_sessions; updated signup/login to handle new sign() return type
- **migrations/0011_session_revocation.sql**: New migration for revoked_sessions table

### Testing
- **tests/auth_flow.rs**: Added 5 new tests for session revocation scenarios
  - `bearer_token_works_before_logout`
  - `bearer_token_rejected_after_logout`
  - `cookie_token_rejected_after_logout`
  - `session_revocation_survives_app_restart`
  - `session_revocation_with_error_code`

### Documentation
- **API.md**: Updated `/logout` endpoint documentation; noted session_id in JWT claims
- **SESSION_REVOCATION.md**: Comprehensive implementation guide with schema, architecture, and behavior details

### Test Configuration
- **tests/common/mod.rs**: Updated AppState initialization to include login_rate_limiter and settlement config (required by AppState)
- **src/lib.rs**: Exported LoginRateLimitConfig, SettlementConfig, IssuerConfig for tests

## Test Results

All 28 existing and new authentication tests pass:
- 14 auth_flow tests (9 existing + 5 new)
- 10 auth_rate_limiting tests
- 4 wallet_flow tests

## How It Works

1. **Token issuance**: When a user signs up or logs in, a unique `session_id` is generated and embedded in the JWT
2. **Request validation**: On every authenticated request, the extractor verifies the token signature and checks if the session_id exists in the revoked_sessions table
3. **Logout**: When a user logs out with a Bearer token, the session_id is extracted and inserted into revoked_sessions with an expiry timestamp
4. **Cookie clearing**: The session cookie is cleared immediately for cookie-based clients
5. **Persistence**: The revoked session entry persists in the database across restarts
6. **Cleanup**: Expired entries (older than 24 hours) can be safely deleted via scheduled cleanup jobs

## Security Properties

- **No token reuse**: Once revoked, a token cannot be used again, even if copied elsewhere
- **Persistence**: Revocation survives application restarts
- **Backward compatible**: 24-hour token expiry acts as a backstop for sessions not explicitly revoked
- **Error masking**: Revoked tokens return the same error as expired tokens, preventing information leakage
- **Cookie security**: HttpOnly cookies are cleared immediately; bearer clients can only reuse if they copied the token explicitly
