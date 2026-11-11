//! Redis-backed rate limiter using atomic Lua script (INCR + EXPIRE).
//!
//! Implements the [`RateLimiter`] trait from `buzz-auth`.
//! Uses a single Lua script to atomically INCR and conditionally EXPIRE,
//! eliminating the crash window where a key could exist without a TTL.
//!
//! ⚠️ Fixed windows allow up to 2× burst at boundaries. Upgrade to sliding
//! window or token bucket for strict limiting.

use std::net::IpAddr;

use buzz_auth::{
    error::AuthError,
    rate_limit::{LimitType, RateLimitResult, RateLimiter},
};
use buzz_core::TenantContext;
use nostr::PublicKey;
use redis::Script;

/// Atomically INCR the key, set EXPIRE on first call, and return (count, ttl).
///
/// Using a Lua script ensures INCR and EXPIRE are executed atomically —
/// a crash between them can no longer leave a key without a TTL.
const RATE_LIMIT_SCRIPT: &str = r#"
local count = redis.call('INCR', KEYS[1])
if count == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
local ttl = redis.call('TTL', KEYS[1])
return {count, ttl}
"#;

/// Run the atomic rate-limit Lua script against `key` and return a
/// [`RateLimitResult`].
///
/// If the TTL comes back negative (key exists without expiry — broken state
/// from a prior crash), the key is repaired with a fresh EXPIRE and a warning
/// is logged.
async fn run_rate_limit(
    pool: &crate::RedisConnection,
    key: &str,
    window_secs: u64,
    limit: u64,
) -> Result<RateLimitResult, AuthError> {
    let mut conn = pool.clone();

    let script = Script::new(RATE_LIMIT_SCRIPT);
    let (count, ttl): (u64, i64) = script
        .key(key)
        .arg(window_secs as i64)
        .invoke_async(&mut conn)
        .await
        .map_err(|e| AuthError::Internal(format!("Redis rate limit script: {e}")))?;

    // ttl == -1 means the key exists but has no expiry — broken state from a
    // prior crash between INCR and EXPIRE. Repair it now.
    let reset_in_secs = if ttl < 0 {
        tracing::warn!(key = %key, "rate limit key has no TTL — repairing");
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(window_secs as i64)
            .query_async(&mut conn)
            .await
            .map_err(|e| AuthError::Internal(format!("Redis EXPIRE repair: {e}")))?;
        // After repair, the window resets to the full duration.
        window_secs
    } else {
        ttl.max(0) as u64
    };

    if count <= limit {
        Ok(RateLimitResult::allowed(count, limit, reset_in_secs))
    } else {
        Ok(RateLimitResult::denied(count, limit, reset_in_secs))
    }
}

/// Redis-backed rate limiter using fixed-window counters.
///
/// Pubkey keys are community-scoped via `&TenantContext`:
/// `buzz:{community}:ratelimit:{pubkey_hex}:{suffix}`. IP keys remain
/// operator-global: `buzz:ratelimit:ip:{ip}:conn`. The counter and its TTL are
/// managed atomically via a Lua script to prevent keys from persisting without
/// expiry.
pub struct RedisRateLimiter {
    pool: crate::RedisConnection,
}

impl RedisRateLimiter {
    /// Create a new `RedisRateLimiter` backed by the given connection pool.
    pub fn new(pool: crate::RedisConnection) -> Self {
        Self { pool }
    }

    /// Increment a caller-constructed named counter through the shared Lua script.
    ///
    /// App callbacks use the exact key
    /// `buzz:{community}:ratelimit:app_callback:{app_uuid}:{peer_ip}` so the
    /// counter is shared across relay processes. The 60-request / 60-second
    /// default is applied by the callback caller, not here.
    pub async fn check_named_key(
        &self,
        key: &str,
        window_secs: u64,
        limit: u64,
    ) -> Result<RateLimitResult, AuthError> {
        run_rate_limit(&self.pool, key, window_secs, limit).await
    }
}

impl RateLimiter for RedisRateLimiter {
    async fn check_and_increment(
        &self,
        ctx: &TenantContext,
        pubkey: &PublicKey,
        limit_type: LimitType,
        window_secs: u64,
        limit: u64,
    ) -> Result<RateLimitResult, AuthError> {
        let key = buzz_auth::rate_limit::rate_limit_key(ctx, pubkey, &limit_type);
        run_rate_limit(&self.pool, &key, window_secs, limit).await
    }

    async fn check_ip_connection(
        &self,
        ip: &IpAddr,
        window_secs: u64,
        limit: u64,
    ) -> Result<RateLimitResult, AuthError> {
        let key = buzz_auth::rate_limit::ip_rate_limit_key(ip);
        run_rate_limit(&self.pool, &key, window_secs, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_core::CommunityId;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;

    #[tokio::test]
    #[ignore = "requires Redis"]
    async fn app_callback_rate_limit_key_is_fully_scoped() {
        let community_a = CommunityId::from_uuid(
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("community a"),
        );
        let community_b = CommunityId::from_uuid(
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("community b"),
        );
        let app_a = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").expect("app a");
        let app_b = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").expect("app b");
        let ip_a = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let ip_b = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));

        let base = format!("buzz:{community_a}:ratelimit:app_callback:{app_a}:{ip_a}");
        assert_eq!(
            base,
            "buzz:11111111-1111-1111-1111-111111111111:ratelimit:app_callback:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa:192.0.2.10"
        );
        let different_community =
            format!("buzz:{community_b}:ratelimit:app_callback:{app_a}:{ip_a}");
        let different_app = format!("buzz:{community_a}:ratelimit:app_callback:{app_b}:{ip_a}");
        let different_ip = format!("buzz:{community_a}:ratelimit:app_callback:{app_a}:{ip_b}");
        assert_ne!(base, different_community);
        assert_ne!(base, different_app);
        assert_ne!(base, different_ip);

        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
        let pool = crate::redis_connection(&redis_url).expect("redis connection");
        let limiter = RedisRateLimiter::new(pool.clone());
        for key in [&base, &different_community, &different_app, &different_ip] {
            limiter
                .check_named_key(key, 60, 60)
                .await
                .expect("named App callback key must be admitted through the shared limiter");
            let mut conn = pool.clone();
            let exists: i64 = redis::cmd("EXISTS")
                .arg(key)
                .query_async(&mut conn)
                .await
                .expect("exists");
            assert_eq!(exists, 1, "exact key {key} must be the Redis counter");
            let _: () = redis::cmd("DEL")
                .arg(key)
                .query_async(&mut conn)
                .await
                .expect("cleanup");
        }
    }
}
