use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};

pub struct ResourceGovernor {
    concurrent_semaphore: Arc<Semaphore>,
    current_concurrent: AtomicUsize,
    token_budget_per_minute: u64,
    tokens_used_this_minute: Arc<AtomicU64>,
    _reset_abort: AbortHandle,
}

impl ResourceGovernor {
    pub fn new(max_concurrent: usize, token_budget: u64) -> Self {
        let tokens_used = Arc::new(AtomicU64::new(0));
        let reset_handle = spawn_token_reset(tokens_used.clone());
        let _reset_abort = reset_handle.abort_handle();
        Self {
            concurrent_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            current_concurrent: AtomicUsize::new(0),
            token_budget_per_minute: token_budget,
            tokens_used_this_minute: tokens_used,
            _reset_abort,
        }
    }

    pub async fn acquire_call_permit(&self) -> CallPermit<'_> {
        let permit = self
            .concurrent_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Semaphore closed");
        self.current_concurrent
            .fetch_add(1, Ordering::Relaxed);
        CallPermit {
            _permit: permit,
            governor: self,
            tokens_consumed: 0,
        }
    }

    pub fn try_spend_tokens(&self, count: u64) -> bool {
        let prev = self.tokens_used_this_minute.fetch_add(count, Ordering::SeqCst);
        if prev + count > self.token_budget_per_minute {
            self.tokens_used_this_minute.fetch_sub(count, Ordering::SeqCst);
            false
        } else {
            true
        }
    }

    pub fn tokens_available(&self) -> u64 {
        let used = self.tokens_used_this_minute.load(Ordering::SeqCst);
        self.token_budget_per_minute.saturating_sub(used)
    }

    pub fn active_calls(&self) -> usize {
        self.current_concurrent.load(Ordering::Relaxed)
    }

    pub fn shutdown(&self) {
        self._reset_abort.abort();
    }
}

fn spawn_token_reset(tokens_used: Arc<AtomicU64>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            tokens_used.store(0, Ordering::SeqCst);
        }
    })
}

pub struct CallPermit<'a> {
    _permit: OwnedSemaphorePermit,
    governor: &'a ResourceGovernor,
    tokens_consumed: u64,
}

impl<'a> CallPermit<'a> {
    pub fn record_tokens(&mut self, count: u64) {
        self.tokens_consumed += count;
        self.governor.try_spend_tokens(count);
    }
}

impl<'a> Drop for CallPermit<'a> {
    fn drop(&mut self) {
        self.governor
            .current_concurrent
            .fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_and_release_permit() {
        let governor = ResourceGovernor::new(2, 1000);
        let permit1 = governor.acquire_call_permit().await;
        let permit2 = governor.acquire_call_permit().await;
        assert_eq!(governor.active_calls(), 2);
        drop(permit1);
        assert_eq!(governor.active_calls(), 1);
        drop(permit2);
        assert_eq!(governor.active_calls(), 0);
    }

    #[tokio::test]
    async fn test_token_budget() {
        let governor = ResourceGovernor::new(10, 1000);
        {
            let mut permit = governor.acquire_call_permit().await;
            permit.record_tokens(500);
        }
        {
            let mut permit = governor.acquire_call_permit().await;
            permit.record_tokens(500);
        }
        assert_eq!(governor.tokens_available(), 0);
        assert!(!governor.try_spend_tokens(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_token_spending() {
        let gov = Arc::new(ResourceGovernor::new(10, 1000));
        let mut handles = vec![];
        for _ in 0..5 {
            let gov = gov.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..10 {
                    gov.try_spend_tokens(20);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // Should not exceed budget (5x10x20 = 1000 exactly)
        assert!(gov.tokens_available() <= 1000);
    }
}
