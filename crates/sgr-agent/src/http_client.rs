//! The HTTP client every LLM provider talks through.
//!
//! `reqwest::Client::new()` has no timeouts of any kind, and on a phone that
//! is not a small omission. A pooled connection that has been idle for a
//! minute is usually already dead — a carrier NAT or a Wi-Fi handover drops it
//! without telling either end — and reusing it produces a request that never
//! completes and never errors. Measured on an iPhone simulator: the first
//! question asked after the app had been sitting idle hung for exactly 120
//! seconds, which was the agent's own deadline firing, and the next question
//! answered in 2.4s. Two minutes of nothing, then a normal answer.
//!
//! So the three limits below are what make a dead socket look like a failure
//! rather than a wait. Retry already treats timeouts and connect errors as
//! transient (`retry::is_retryable`), so a stale connection now costs one
//! reconnect instead of the whole turn.
//!
//! # It is not only our socket — the gateway drops the first request too
//!
//! Measured 18 Aug 2026 with `curl` from a laptop, no app and no pool involved,
//! against the Cloudflare AI Gateway (`workers-ai/@cf/google/gemma-4-26b`),
//! a 22 KB prompt and a fresh connection for every call:
//!
//! | after 70s of quiet | 90s deadline, **no answer at all** (`http_code 000`) |
//! | immediately after  | 0.9s |
//! | again              | 0.2s |
//!
//! Three fresh connections, one of them lost. So the request that disappears is
//! not a stale socket being written into: the provider's cold path swallows it,
//! and nothing on this side can prevent that. What this side controls is how
//! long someone waits before the retry saves the turn — which is the number
//! below, and why it is a ceiling rather than a guess at how slow an answer can
//! be. In the phone's own logs the shape is unmistakable: two calls at 16–17s
//! inside one turn, then every later call on the same connection at 1.6–2.5s.

use std::time::Duration;

/// Give up on a connection that will not open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The ceiling for one request, response included.
///
/// Set against measurement rather than taste: turns against Workers AI take
/// 2–11 seconds, and the pathological case — the very first connection a
/// freshly launched process opens — does not answer at all. It hangs until
/// something cuts it off, and the retry that follows succeeds in about two
/// seconds. So this number is very nearly the whole cost of a first question:
/// at 90s the first turn measured 92s, and every later turn 15s.
///
/// The clients built by `openai-oxide` cannot be handed a `reqwest::Client`,
/// so they take this same number through `ClientConfig::timeout_secs` — the
/// library's own default is 600s, which on a phone means a dead connection
/// holds the whole conversation for ten minutes before anyone is told.
///
/// And they keep their own pool, at `pool_idle_timeout(300s)` — five minutes,
/// where the network forgets a socket in about one. So this number is not only
/// a ceiling on a slow answer, it is the **price of a dead connection**, and it
/// is paid whenever someone comes back to the app after a pause. Measured on
/// 17 Aug: 258 turns, median 1.8s, p90 6.9s — and a cluster sitting at 29–35s,
/// which is this ceiling plus the retry that then answers in about two.
///
/// Fifteen is still comfortably above the slowest real answer (11s measured)
/// and halves what the stale socket costs: 17s instead of 32s before the reply
/// arrives. The retry is what actually fixes it; this only decides how long
/// someone stares at a spinner first.
pub const REQUEST_TIMEOUT_SECS: u64 = 15;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(REQUEST_TIMEOUT_SECS);

/// How long an unused connection may stay in the pool.
///
/// Shorter than the minute or so a mobile network takes to forget it, so the
/// client opens a fresh one instead of writing into a socket nothing is
/// listening to.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// A client configured for talking to an LLM over a network that comes and goes.
pub fn build() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .build()
        // A builder failure means the TLS backend is unavailable, which the
        // default constructor would panic on too. Falling back keeps the old
        // behaviour rather than turning a working setup into a crash.
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    /// The limits are the point of this module — a client without them is the
    /// bug it was written for, so their presence is asserted rather than left
    /// to whoever edits `build()` next.
    #[test]
    fn the_limits_are_ordered_the_way_a_phone_needs_them() {
        assert!(
            super::CONNECT_TIMEOUT < super::REQUEST_TIMEOUT,
            "a connection that will not open must fail long before a slow answer does"
        );
        assert!(
            super::POOL_IDLE_TIMEOUT < std::time::Duration::from_secs(60),
            "a mobile network forgets an idle connection in about a minute; \
                 the pool has to forget it first"
        );
    }

    /// The ceiling is what a dead socket costs before the retry saves the turn,
    /// so it has to stay in sight of a real answer rather than of patience.
    /// Slowest measured answer is 11s; anything much above 20 turns "the app
    /// froze" into the ordinary experience of coming back to it.
    #[test]
    fn the_request_ceiling_is_the_price_of_a_stale_connection() {
        assert!(
            (12..=20).contains(&super::REQUEST_TIMEOUT_SECS),
            "above the slowest real answer, below what reads as a hang"
        );
    }

    #[test]
    fn a_client_is_built() {
        let _ = super::build();
    }
}
