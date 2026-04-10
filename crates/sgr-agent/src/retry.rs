//! RetryClient — wraps LlmClient with exponential backoff for transient errors.
//!
//! Retries on: rate limits (429), server errors (5xx), empty responses, network errors.
//! Honors `retry_after_secs` from rate limit headers when available.

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{Message, SgrError, ToolCall};
use serde_json::Value;
use std::time::Duration;

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Max retry attempts (0 = no retries).
    pub max_retries: usize,
    /// Base delay in milliseconds.
    pub base_delay_ms: u64,
    /// Max delay cap in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
        }
    }
}

/// Determine if an error is retryable (transient: rate limit, timeout, server errors).
pub fn is_retryable(err: &SgrError) -> bool {
    match err {
        SgrError::RateLimit { .. } => true,
        SgrError::EmptyResponse => true,
        // reqwest::Error — retryable if timeout or connect error
        SgrError::Http(e) => e.is_timeout() || e.is_connect() || e.is_request(),
        SgrError::Api { status, .. } => *status == 0 || *status >= 500 || *status == 408 || *status == 429,
        // Empty response wrapped as Schema error — transient model behavior
        SgrError::Schema(msg) => msg.contains("Empty response"),
        // MaxOutputTokens and PromptTooLong are NOT retryable at this level —
        // they are handled by the agent loop with special recovery logic
        SgrError::MaxOutputTokens { .. } | SgrError::PromptTooLong(_) => false,
        _ => false,
    }
}

/// Calculate delay for attempt N, honoring rate limit headers.
pub fn delay_for_attempt(attempt: usize, config: &RetryConfig, err: &SgrError) -> Duration {
    // Honor retry-after header from rate limit
    if let Some(info) = err.rate_limit_info()
        && let Some(secs) = info.retry_after_secs
    {
        return Duration::from_secs(secs + 1); // +1s safety margin
    }

    // Exponential backoff: base * 2^attempt, capped at max
    let delay_ms = (config.base_delay_ms * (1 << attempt)).min(config.max_delay_ms);
    // Add jitter ±10%
    let jitter = (delay_ms as f64 * 0.1 * (attempt as f64 % 2.0 - 0.5)) as u64;
    Duration::from_millis(delay_ms.saturating_add(jitter))
}

/// LLM client wrapper with automatic retry on transient errors.
pub struct RetryClient<C: LlmClient> {
    inner: C,
    config: RetryConfig,
}

impl<C: LlmClient> RetryClient<C> {
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            config: RetryConfig::default(),
        }
    }

    pub fn with_config(mut self, config: RetryConfig) -> Self {
        self.config = config;
        self
    }
}

#[async_trait::async_trait]
impl<C: LlmClient> LlmClient for RetryClient<C> {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        let mut last_err = None;
        for attempt in 0..=self.config.max_retries {
            match self.inner.structured_call(messages, schema).await {
                Ok(result) => return Ok(result),
                Err(e) if is_retryable(&e) && attempt < self.config.max_retries => {
                    let delay = delay_for_attempt(attempt, &self.config, &e);
                    tracing::warn!(
                        attempt = attempt + 1,
                        max = self.config.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying structured_call: {}",
                        e
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap())
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let mut last_err = None;
        for attempt in 0..=self.config.max_retries {
            match self.inner.tools_call(messages, tools).await {
                Ok(result) => return Ok(result),
                Err(e) if is_retryable(&e) && attempt < self.config.max_retries => {
                    let delay = delay_for_attempt(attempt, &self.config, &e);
                    tracing::warn!(
                        attempt = attempt + 1,
                        max = self.config.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying tools_call: {}",
                        e
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap())
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let mut last_err = None;
        for attempt in 0..=self.config.max_retries {
            match self.inner.complete(messages).await {
                Ok(result) => return Ok(result),
                Err(e) if is_retryable(&e) && attempt < self.config.max_retries => {
                    let delay = delay_for_attempt(attempt, &self.config, &e);
                    tracing::warn!(
                        attempt = attempt + 1,
                        max = self.config.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying complete: {}",
                        e
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingClient {
        fail_count: usize,
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LlmClient for FailingClient {
        async fn structured_call(
            &self,
            _: &[Message],
            _: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_count {
                Err(SgrError::EmptyResponse)
            } else {
                Ok((None, vec![], "ok".into()))
            }
        }
        async fn tools_call(
            &self,
            _: &[Message],
            _: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_count {
                Err(SgrError::Api {
                    status: 500,
                    body: "internal error".into(),
                })
            } else {
                Ok(vec![])
            }
        }
        async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn retries_on_empty_response() {
        let count = Arc::new(AtomicUsize::new(0));
        let client = RetryClient::new(FailingClient {
            fail_count: 2,
            call_count: count.clone(),
        })
        .with_config(RetryConfig {
            max_retries: 3,
            base_delay_ms: 1,
            max_delay_ms: 10,
        });

        let result = client
            .structured_call(&[Message::user("hi")], &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(count.load(Ordering::SeqCst), 3); // 2 fails + 1 success
    }

    #[tokio::test]
    async fn retries_on_server_error() {
        let count = Arc::new(AtomicUsize::new(0));
        let client = RetryClient::new(FailingClient {
            fail_count: 1,
            call_count: count.clone(),
        })
        .with_config(RetryConfig {
            max_retries: 2,
            base_delay_ms: 1,
            max_delay_ms: 10,
        });

        let result = client.tools_call(&[Message::user("hi")], &[]).await;
        assert!(result.is_ok());
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fails_after_max_retries() {
        let count = Arc::new(AtomicUsize::new(0));
        let client = RetryClient::new(FailingClient {
            fail_count: 10,
            call_count: count.clone(),
        })
        .with_config(RetryConfig {
            max_retries: 2,
            base_delay_ms: 1,
            max_delay_ms: 10,
        });

        let result = client
            .structured_call(&[Message::user("hi")], &serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[test]
    fn non_retryable_errors() {
        assert!(!is_retryable(&SgrError::Api {
            status: 400,
            body: "bad request".into()
        }));
        assert!(!is_retryable(&SgrError::Schema("parse".into())));
        assert!(is_retryable(&SgrError::Schema(
            "Empty response from model (parts: text)".into()
        )));
        assert!(is_retryable(&SgrError::EmptyResponse));
        assert!(is_retryable(&SgrError::Api {
            status: 503,
            body: "server error".into()
        }));
        assert!(is_retryable(&SgrError::Api {
            status: 429,
            body: "rate limit".into()
        }));
    }

    #[test]
    fn delay_exponential_backoff() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay_ms: 100,
            max_delay_ms: 5000,
        };
        let err = SgrError::EmptyResponse;

        let d0 = delay_for_attempt(0, &config, &err);
        let d1 = delay_for_attempt(1, &config, &err);
        let d2 = delay_for_attempt(2, &config, &err);

        // Roughly 100ms, 200ms, 400ms (with jitter)
        assert!(d0.as_millis() <= 150);
        assert!(d1.as_millis() <= 250);
        assert!(d2.as_millis() <= 500);
    }

    #[test]
    fn delay_capped_at_max() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay_ms: 1000,
            max_delay_ms: 5000,
        };
        let err = SgrError::EmptyResponse;

        let d10 = delay_for_attempt(10, &config, &err);
        assert!(d10.as_millis() <= 5500); // max + jitter
    }
}
