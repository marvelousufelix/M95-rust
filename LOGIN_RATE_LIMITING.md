# Login Rate Limiting

## Overview

The login endpoint includes built-in rate limiting to protect against password guessing and brute force attacks. Rate limiting is applied consistently to all login attempts (both valid and invalid emails/passwords) without revealing whether an email address is registered.

## Configuration

Rate limiting is controlled via environment variables:

| Variable | Default | Description |
|---|---|---|
| `LOGIN_RATE_LIMIT_MAX_ATTEMPTS` | `5` | Maximum login attempts allowed within the window |
| `LOGIN_RATE_LIMIT_WINDOW_SECS` | `300` | Time window in seconds (5 minutes) |

### Examples

```bash
# Tighter security: 3 attempts per 5 minutes
LOGIN_RATE_LIMIT_MAX_ATTEMPTS=3
LOGIN_RATE_LIMIT_WINDOW_SECS=300

# More lenient: 10 attempts per 15 minutes
LOGIN_RATE_LIMIT_MAX_ATTEMPTS=10
LOGIN_RATE_LIMIT_WINDOW_SECS=900
```

## How It Works

### Sliding Window Algorithm

The rate limiter uses a **sliding window** approach:

1. **Attempt Tracking** — Each login attempt (success or failure) is recorded with a timestamp
2. **Window Pruning** — Old attempts outside the configured window are discarded
3. **Threshold Check** — If the number of remaining attempts reaches the limit, subsequent requests are throttled

Example with default config (5 attempts per 300 seconds):
- Attempts at: 10:00, 10:05, 10:10, 10:15, 10:20 → allowed (5 total)
- Attempt at: 10:22 → **throttled** (would be 6th in the window)
- Attempt at: 10:35 → **allowed** (oldest attempt from 10:00 has aged out)

### Privacy-Conscious Keying

The rate limiter uses a **hashed email** as the tracking key, never storing or logging raw email addresses:

```
Request email → Hash (SHA-256) → Storage key
```

This design ensures:
- **No email enumeration:** Unknown emails and wrong passwords look identical to rate limiting
- **No email leakage:** Raw email addresses never appear in logs or memory
- **Consistent protection:** Both valid and invalid login attempts are rate-limited equally

## API Responses

### Under Limit (Allowed)

```http
POST /login
Content-Type: application/json

{
  "email": "user@example.com",
  "password": "password"
}

HTTP/1.1 200 OK
Set-Cookie: session=...
Content-Type: application/json

{
  "token": "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9...",
  "user_id": "550e8400-e29b-41d4-a716-446655440000",
  "merchant_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
}
```

### Over Limit (Throttled)

When rate limit is exceeded, the server responds with **HTTP 429 Too Many Requests**:

```http
POST /login
Content-Type: application/json

{
  "email": "user@example.com",
  "password": "password"
}

HTTP/1.1 429 Too Many Requests
Retry-After: 247
Content-Type: application/json

{
  "error": "too many login attempts, please try again later",
  "code": "TOO_MANY_REQUESTS"
}
```

**Key Headers:**
- `Retry-After: 247` — Client should wait 247 seconds before retrying

## Error Codes

All login failures return `401 Unauthorized` with consistent messaging to prevent email enumeration:

| Scenario | Status | Code | Message |
|---|---|---|---|
| Invalid email format | 400 | `INVALID_PARAMETERS` | `must be a valid email address` |
| Email not found | 401 | `INVALID_CREDENTIALS` | `invalid email or password` |
| Wrong password | 401 | `INVALID_CREDENTIALS` | `invalid email or password` |
| Rate limited | 429 | `TOO_MANY_REQUESTS` | `too many login attempts, please try again later` |

Note: Unknown email and wrong password responses are intentionally identical (`INVALID_CREDENTIALS`) to prevent attackers from enumerating valid email addresses.

## Client Behavior

### Recommended Handling

Clients should implement the following retry logic:

```javascript
async function loginWithRetry(email, password) {
  try {
    const response = await fetch('/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, password })
    });

    if (response.status === 429) {
      // Rate limited
      const retryAfter = response.headers.get('Retry-After');
      const waitSeconds = parseInt(retryAfter) || 60;
      console.log(`Please try again in ${waitSeconds} seconds`);
      
      // Show user-friendly message with wait time
      showErrorMessage(`Too many login attempts. Please try again in ${waitSeconds} seconds.`);
      return null;
    }

    if (!response.ok) {
      const error = await response.json();
      console.log(`Login failed: ${error.error}`);
      return null;
    }

    return await response.json();
  } catch (err) {
    console.error('Login error:', err);
    return null;
  }
}
```

### User-Facing Messaging

Suggested UX when rate limited:

```
❌ Too many login attempts

You've made too many login attempts. Please try again in 4 minutes and 52 seconds.

[Retry Login Button - disabled until countdown complete]
```

## Security Considerations

### Protection Against

✅ **Brute force password guessing** — Attackers cannot try many passwords rapidly  
✅ **Email enumeration** — Cannot determine if an email is registered  
✅ **Distributed attacks** — Rate limiting is per-email, not per-IP (IPs aren't always unique; email is)

### Limitations

⚠️ **In-memory storage** — Rate limiting data is stored in RAM; restarting the process clears the state  
⚠️ **Single-instance only** — Distributed deployments need a shared cache (Redis) if exact consistency matters  
⚠️ **No secondary verification** — Does not prevent legitimate users who forget passwords (consider password reset flow separately)

### Deployment Notes

For high-availability deployments:
1. **Single instance:** Current in-memory approach works fine
2. **Multiple instances behind load balancer:** Each instance has independent rate limiting. Different logins may hit different instances, spreading attempts. This is acceptable for most use cases.
3. **Strict enforcement across fleet:** Consider moving rate limiting to a shared cache (Redis) in the future

## Testing

The implementation includes comprehensive tests verifying:

- ✅ Throttling activates after max attempts
- ✅ Valid logins work below the limit
- ✅ Rate limit is per-email (different emails don't interfere)
- ✅ Successful login resets the counter
- ✅ `Retry-After` header is included in 429 response
- ✅ Unknown email and wrong password are indistinguishable

Run tests:
```bash
cargo test auth_rate_limiting
```

## Troubleshooting

### "Too many requests" even after a long time

**Cause:** The window only resets when old attempts expire (based on wall-clock time).

**Solution:** Either wait for the full window to pass, or have an admin reset (not currently implemented—would require database access or special endpoint).

### Rate limiting is too strict/lenient

**Solution:** Adjust environment variables and restart:
```bash
# More lenient
LOGIN_RATE_LIMIT_MAX_ATTEMPTS=10
LOGIN_RATE_LIMIT_WINDOW_SECS=600  # 10 minutes

# Stricter
LOGIN_RATE_LIMIT_MAX_ATTEMPTS=3
LOGIN_RATE_LIMIT_WINDOW_SECS=180  # 3 minutes
```

### I locked myself out by accident

**Current:** No admin reset available yet. Workaround: wait for the window to expire, or restart the service to clear in-memory state.

**Future:** Implement admin endpoint to reset rate limits for a specific email.
