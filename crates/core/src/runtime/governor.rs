use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct ResourceGovernor {
    concurrent_semaphore: Arc<Semaphore>,
    current_concurrent: AtomicUsize,
    token_budget_per_minute: u64,
    tokens_used_this_minute: Arc<AtomicU64>,
}

impl ResourceGovernor {
    pub fn new(max_concurrent: usize, token_budget: u64) -> Self {
        let tokens_used = Arc::new(AtomicU64::new(0));
        spawn_token_reset(tokens_used.clone());
        Self {
            concurrent_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            current_concurrent: AtomicUsize::new(0),
            token_budget_per_minute: token_budget,
            tokens_used_this_minute: tokens_used,
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

    pub fn can_spend_tokens(&self, count: u64) -> bool {
        self.tokens_available() >= count
    }

    pub fn record_tokens(&self, count: u64) {
        self.tokens_used_this_minute
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn tokens_available(&self) -> u64 {
        let used = self.tokens_used_this_minute.load(Ordering::Relaxed);
        self.token_budget_per_minute.saturating_sub(used)
    }

    pub fn active_calls(&self) -> usize {
        self.current_concurrent.load(Ordering::Relaxed)
    }
}

fn spawn_token_reset(tokens_used: Arc<AtomicU64>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            tokens_used.store(0, Ordering::Relaxed);
        }
    });
}

pub struct CallPermit<'a> {
    _permit: OwnedSemaphorePermit,
    governor: &'a ResourceGovernor,
    tokens_consumed: u64,
}

impl<'a> CallPermit<'a> {
    pub fn record_tokens(&mut self, count: u64) {
        self.tokens_consumed += count;
        self.governor.record_tokens(count);
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
        assert!(!governor.can_spend_tokens(1));
    }
}
