use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

struct ResetSignal {
    stop: Mutex<bool>,
    wake: Condvar,
}

pub struct ResourceGovernor {
    concurrent_semaphore: Arc<Semaphore>,
    current_concurrent: AtomicUsize,
    token_budget_per_minute: u64,
    tokens_used_this_minute: Arc<AtomicU64>,
    reset_signal: Arc<ResetSignal>,
    reset_thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(test)]
    reset_thread_finished: Arc<AtomicBool>,
}

impl ResourceGovernor {
    pub fn new(max_concurrent: usize, token_budget: u64) -> Self {
        let tokens_used = Arc::new(AtomicU64::new(0));
        let reset_signal = Arc::new(ResetSignal {
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        #[cfg(test)]
        let reset_thread_finished = Arc::new(AtomicBool::new(false));

        let tokens = tokens_used.clone();
        let signal = reset_signal.clone();
        #[cfg(test)]
        let finished = reset_thread_finished.clone();
        let handle = std::thread::spawn(move || {
            let mut stop = signal.stop.lock().unwrap();
            loop {
                if *stop {
                    #[cfg(test)]
                    finished.store(true, Ordering::SeqCst);
                    break;
                }
                let (next_stop, _) = signal
                    .wake
                    .wait_timeout(stop, Duration::from_secs(60))
                    .unwrap();
                stop = next_stop;
                if *stop {
                    #[cfg(test)]
                    finished.store(true, Ordering::SeqCst);
                    break;
                }
                tokens.store(0, Ordering::SeqCst);
            }
        });

        Self {
            concurrent_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            current_concurrent: AtomicUsize::new(0),
            token_budget_per_minute: token_budget,
            tokens_used_this_minute: tokens_used,
            reset_signal,
            reset_thread: Some(handle),
            #[cfg(test)]
            reset_thread_finished,
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

    pub fn try_reserve_tokens(&self, count: u64) -> bool {
        let mut current = self.tokens_used_this_minute.load(Ordering::SeqCst);
        loop {
            let Some(next) = current.checked_add(count) else {
                return false;
            };
            if next > self.token_budget_per_minute {
                return false;
            }

            match self.tokens_used_this_minute.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    pub fn release_tokens(&self, count: u64) {
        let mut current = self.tokens_used_this_minute.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_sub(count);
            match self.tokens_used_this_minute.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub fn try_spend_tokens(&self, count: u64) -> bool {
        self.try_reserve_tokens(count)
    }

    pub fn tokens_available(&self) -> u64 {
        let used = self.tokens_used_this_minute.load(Ordering::SeqCst);
        self.token_budget_per_minute.saturating_sub(used)
    }

    pub fn active_calls(&self) -> usize {
        self.current_concurrent.load(Ordering::Relaxed)
    }

    pub fn shutdown(&self) {
        let mut stop = self.reset_signal.stop.lock().unwrap();
        *stop = true;
        self.reset_signal.wake.notify_one();
    }
}

impl Drop for ResourceGovernor {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.reset_thread.take() {
            let _ = handle.join();
        }
    }
}

pub struct CallPermit<'a> {
    _permit: OwnedSemaphorePermit,
    governor: &'a ResourceGovernor,
    tokens_consumed: u64,
}

impl<'a> CallPermit<'a> {
    pub fn record_tokens(&mut self, count: u64) {
        self.tokens_consumed = self.tokens_consumed.saturating_add(count);
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

    #[test]
    fn shutdown_joins_reset_thread_promptly() {
        let governor = ResourceGovernor::new(1, 1);
        let finished = governor.reset_thread_finished.clone();

        governor.shutdown();
        drop(governor);

        assert!(finished.load(Ordering::SeqCst));
    }

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

    #[test]
    fn test_token_reservation_and_release() {
        let governor = ResourceGovernor::new(1, 10);

        assert!(governor.try_reserve_tokens(7));
        assert_eq!(governor.tokens_available(), 3);
        governor.release_tokens(4);
        assert_eq!(governor.tokens_available(), 7);
        assert!(governor.try_reserve_tokens(7));
        assert!(!governor.try_reserve_tokens(1));
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
        assert!(gov.tokens_available() <= 1000);
    }
}
