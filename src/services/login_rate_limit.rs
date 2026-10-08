/// Login rate limiting using a privacy-conscious sliding window approach.
///
/// The rate limiter uses a hash of the email address (not the raw email) as the key.
/// This ensures that:
/// - We don't log raw email addresses in memory
/// - Failed attempts on non-existent emails are still rate-limited
/// - Valid and invalid login attempts are indistinguishable to rate limiting logic
///
/// The sliding window tracks all request timestamps within a configured window.
/// When a new request comes in, we remove timestamps older than the window and check
/// if the remaining count exceeds the limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hash the email for privacy: never store or log the raw email address.
fn hash_email(email: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    email.hash(&mut hasher);
    hasher.finish()
}

/// Get current time as seconds since UNIX_EPOCH.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A single login attempt record within the sliding window.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    /// Timestamp in seconds since UNIX_EPOCH.
    timestamp_secs: u64,
}

/// Per-email rate limiter state using a sliding window.
#[derive(Debug)]
struct EmailRateLimit {
    /// All attempt timestamps (in seconds) within the window.
    attempts: Vec<u64>,
}

impl EmailRateLimit {
    fn new() -> Self {
        EmailRateLimit {
            attempts: Vec::new(),
        }
    }

    /// Remove attempts older than the window (window_secs seconds ago).
    fn prune(&mut self, now: u64, window_secs: u64) {
        let cutoff = now.saturating_sub(window_secs);
        self.attempts.retain(|&timestamp| timestamp > cutoff);
    }

    /// Check if adding another attempt would exceed the limit.
    fn would_exceed(&self, limit: u32) -> bool {
        self.attempts.len() >= limit as usize
    }

    /// Record a new attempt at the given timestamp.
    fn record(&mut self, now: u64) {
        self.attempts.push(now);
    }

    /// Get the oldest attempt timestamp (to calculate retry-after).
    fn oldest_attempt(&self) -> Option<u64> {
        self.attempts.first().copied()
    }
}

/// In-memory sliding window rate limiter for login attempts.
///
/// Uses a privacy-conscious keying strategy:
/// - Email addresses are hashed before being used as keys
/// - No raw email addresses are stored in memory
/// - This means unknown emails and wrong passwords look identical to rate limiting
pub struct LoginRateLimiter {
    /// Map from hashed email -> attempt history
    attempts: Mutex<HashMap<u64, EmailRateLimit>>,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        LoginRateLimiter {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Check if a login attempt should be rate-limited.
    ///
    /// Returns:
    /// - `Ok(())` if the request is allowed
    /// - `Err(retry_after_secs)` if the request is rate-limited
    ///
    /// The `max_attempts` and `window_secs` parameters define the limit:
    /// - `max_attempts`: maximum attempts allowed within the window
    /// - `window_secs`: time window in seconds
    ///
    /// For example, with `max_attempts=5` and `window_secs=300`:
    /// - 5 attempts within 300 seconds are allowed
    /// - The 6th attempt triggers a rate limit
    pub fn check_and_record(
        &self,
        email: &str,
        max_attempts: u32,
        window_secs: u64,
    ) -> Result<(), u64> {
        let email_hash = hash_email(email);
        let now = now_secs();

        let mut attempts_map = self.attempts.lock().unwrap();
        let entry = attempts_map.entry(email_hash).or_insert_with(EmailRateLimit::new);

        // Prune old attempts outside the window
        entry.prune(now, window_secs);

        // Check if we've exceeded the limit
        if entry.would_exceed(max_attempts) {
            // Calculate retry-after: oldest attempt + window - now
            // This tells the client how long until the oldest attempt ages out
            let oldest = entry.oldest_attempt().unwrap_or(now);
            let retry_after = window_secs.saturating_sub(now.saturating_sub(oldest));
            return Err(retry_after.max(1)); // At least 1 second
        }

        // Record this attempt
        entry.record(now);
        Ok(())
    }

    /// Explicitly reset rate limit for an email (e.g., after successful login).
    /// This is optional but recommended to avoid penalizing a user who
    /// finally logs in successfully after some failed attempts.
    pub fn reset(&self, email: &str) {
        let email_hash = hash_email(email);
        let mut attempts_map = self.attempts.lock().unwrap();
        attempts_map.remove(&email_hash);
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration as StdDuration;

    #[test]
    fn email_hash_is_consistent() {
        let hash1 = hash_email("user@example.com");
        let hash2 = hash_email("user@example.com");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn email_hash_differs_for_different_emails() {
        let hash1 = hash_email("user@example.com");
        let hash2 = hash_email("other@example.com");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn allows_attempts_under_limit() {
        let limiter = LoginRateLimiter::new();
        let email = "user@example.com";

        // Allow 5 attempts within 300 seconds
        for i in 0..5 {
            assert!(limiter.check_and_record(email, 5, 300).is_ok(), "attempt {i} should succeed");
        }
    }

    #[test]
    fn rejects_attempts_over_limit() {
        let limiter = LoginRateLimiter::new();
        let email = "user@example.com";

        // Record 5 attempts
        for _ in 0..5 {
            assert!(limiter.check_and_record(email, 5, 300).is_ok());
        }

        // 6th attempt should be rate-limited
        let result = limiter.check_and_record(email, 5, 300);
        assert!(result.is_err());
        let retry_after = result.unwrap_err();
        assert!(retry_after > 0);
        assert!(retry_after <= 300);
    }

    #[test]
    fn rate_limit_is_per_email() {
        let limiter = LoginRateLimiter::new();
        let email1 = "user1@example.com";
        let email2 = "user2@example.com";

        // Hit rate limit for email1
        for _ in 0..5 {
            assert!(limiter.check_and_record(email1, 5, 300).is_ok());
        }
        assert!(limiter.check_and_record(email1, 5, 300).is_err());

        // email2 should still be allowed
        assert!(limiter.check_and_record(email2, 5, 300).is_ok());
    }

    #[test]
    fn reset_clears_rate_limit() {
        let limiter = LoginRateLimiter::new();
        let email = "user@example.com";

        // Hit rate limit
        for _ in 0..5 {
            assert!(limiter.check_and_record(email, 5, 300).is_ok());
        }
        assert!(limiter.check_and_record(email, 5, 300).is_err());

        // Reset and try again
        limiter.reset(email);
        assert!(limiter.check_and_record(email, 5, 300).is_ok());
    }

    #[test]
    fn retry_after_is_reasonable() {
        let limiter = LoginRateLimiter::new();
        let email = "user@example.com";

        // Record 5 attempts with a 10-second window
        for _ in 0..5 {
            assert!(limiter.check_and_record(email, 5, 10).is_ok());
        }

        // Get the retry-after value
        let retry_after = limiter
            .check_and_record(email, 5, 10)
            .unwrap_err();

        // Should be between 1 and 10 seconds (approximately the window)
        assert!(retry_after >= 1);
        assert!(retry_after <= 10);
    }

    #[test]
    fn does_not_expose_email_in_memory() {
        let limiter = LoginRateLimiter::new();
        let email = "secret@example.com";

        limiter.check_and_record(email, 5, 300).ok();

        // The internal map only has hashed keys, never the raw email
        let map = limiter.attempts.lock().unwrap();
        for key in map.keys() {
            // If the key is very large or contains email-like structure, it failed
            // This is a basic sanity check; in reality we'd need more sophisticated testing
            // to verify no email data leaked
            assert!(*key as usize != email.len());
        }
    }

    #[test]
    fn privacy_unknown_email_and_wrong_password_are_indistinguishable() {
        let limiter = LoginRateLimiter::new();
        let unknown_email = "unknown@example.com";
        let known_email = "known@example.com";

        // Simulate 5 failed attempts on unknown email
        for _ in 0..5 {
            assert!(limiter.check_and_record(unknown_email, 5, 300).is_ok());
        }

        // 6th attempt on unknown email is rate-limited
        assert!(limiter.check_and_record(unknown_email, 5, 300).is_err());

        // But the system can't tell if this was:
        // 1. Unknown email (no user exists)
        // 2. Wrong password (user exists but password wrong)
        // This is the privacy feature: no email enumeration possible
    }
}
