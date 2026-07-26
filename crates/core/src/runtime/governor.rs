use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[cfg(test)]
struct ReserveHook {
    entered: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

struct TokenBucket {
    epoch: u64,
    usage: Mutex<BucketUsage>,
}

struct BucketUsage {
    used: u64,
    exhausted: bool,
}

impl TokenBucket {
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            usage: Mutex::new(BucketUsage {
                used: 0,
                exhausted: false,
            }),
        }
    }

    fn try_reserve(&self, count: u64, limit: u64) -> bool {
        let mut usage = self.usage.lock().unwrap();
        if usage.exhausted {
            return false;
        }

        let Some(next) = usage.used.checked_add(count) else {
            return false;
        };
        if next > limit {
            return false;
        }

        usage.used = next;
        true
    }

    fn release(&self, count: u64) {
        let mut usage = self.usage.lock().unwrap();
        if !usage.exhausted {
            usage.used = usage.used.saturating_sub(count);
        }
    }

    fn exhaust(&self, limit: u64) {
        let mut usage = self.usage.lock().unwrap();
        usage.used = usage.used.max(limit);
        usage.exhausted = true;
    }

    fn used(&self) -> u64 {
        self.usage.lock().unwrap().used
    }
}

struct TokenBudget {
    limit: u64,
    current: Mutex<Arc<TokenBucket>>,
    #[cfg(test)]
    reserve_hook: Mutex<Option<Arc<ReserveHook>>>,
}

impl TokenBudget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            current: Mutex::new(Arc::new(TokenBucket::new(0))),
            #[cfg(test)]
            reserve_hook: Mutex::new(None),
        }
    }

    fn reserve(self: &Arc<Self>, count: u64) -> Option<TokenReservation> {
        // Keep reset from swapping the bucket between selection and increment.
        let current = self.current.lock().unwrap();
        let bucket = current.clone();
        #[cfg(test)]
        if let Some(hook) = self.reserve_hook.lock().unwrap().clone() {
            hook.entered.wait();
            hook.release.wait();
        }
        if bucket.try_reserve(count, self.limit) {
            Some(TokenReservation {
                budget: self.clone(),
                bucket,
                reserved: count,
                armed: true,
            })
        } else {
            None
        }
    }

    fn available(&self) -> u64 {
        let current = self.current.lock().unwrap();
        self.limit.saturating_sub(current.used())
    }

    fn reset(&self) {
        let mut current = self.current.lock().unwrap();
        let next_epoch = current.epoch.saturating_add(1);
        *current = Arc::new(TokenBucket::new(next_epoch));
    }
}

/// A releasable token reservation tied to one accounting window.
pub struct TokenReservation {
    budget: Arc<TokenBudget>,
    bucket: Arc<TokenBucket>,
    reserved: u64,
    armed: bool,
}

impl TokenReservation {
    /// Commit the reservation without releasing its tokens on drop.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Reconcile the reservation with provider-reported usage.
    ///
    /// Returns `false` when the usage cannot fit in the active budget. In that
    /// case the active bucket is exhausted instead of releasing the consumed
    /// reservation.
    pub fn reconcile(&mut self, actual: u64) -> bool {
        if !self.armed {
            return true;
        }

        let current = self.budget.current.lock().unwrap();
        let current_bucket = current.clone();

        if Arc::ptr_eq(&self.bucket, &current_bucket) {
            if actual <= self.reserved {
                self.bucket.release(self.reserved - actual);
                self.reserved = actual;
                self.armed = false;
                return true;
            }

            let extra = actual - self.reserved;
            if self.bucket.try_reserve(extra, self.budget.limit) {
                self.reserved = actual;
                self.armed = false;
                return true;
            }

            self.bucket.exhaust(self.budget.limit);
            self.reserved = 0;
            self.armed = false;
            return false;
        }

        self.bucket.release(self.reserved);
        self.reserved = 0;
        let charged = if actual == 0 {
            true
        } else if current_bucket.try_reserve(actual, self.budget.limit) {
            true
        } else {
            current_bucket.exhaust(self.budget.limit);
            false
        };
        self.armed = false;
        charged
    }
}

impl Drop for TokenReservation {
    fn drop(&mut self) {
        if self.armed {
            self.bucket.release(self.reserved);
        }
    }
}

struct ResetSignal {
    stop: Mutex<bool>,
    wake: Condvar,
}

pub struct ResourceGovernor {
    concurrent_semaphore: Arc<Semaphore>,
    current_concurrent: Arc<AtomicUsize>,
    token_budget: Arc<TokenBudget>,
    reset_signal: Arc<ResetSignal>,
    reset_thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(test)]
    reset_thread_finished: Arc<AtomicBool>,
}

impl ResourceGovernor {
    pub fn new(max_concurrent: usize, token_budget: u64) -> Self {
        let token_budget = Arc::new(TokenBudget::new(token_budget));
        let reset_signal = Arc::new(ResetSignal {
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        #[cfg(test)]
        let reset_thread_finished = Arc::new(AtomicBool::new(false));

        let tokens = token_budget.clone();
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
                tokens.reset();
            }
        });

        Self {
            concurrent_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            current_concurrent: Arc::new(AtomicUsize::new(0)),
            token_budget,
            reset_signal,
            reset_thread: Some(handle),
            #[cfg(test)]
            reset_thread_finished,
        }
    }

    pub async fn acquire_call_permit(&self) -> CallPermit {
        let permit = self
            .concurrent_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Semaphore closed");
        self.current_concurrent.fetch_add(1, Ordering::Relaxed);
        CallPermit {
            _permit: permit,
            current_concurrent: self.current_concurrent.clone(),
            token_budget: self.token_budget.clone(),
            tokens_consumed: 0,
        }
    }

    /// Reserve tokens until the returned guard is reconciled, disarmed, or dropped.
    pub fn reserve_tokens(&self, count: u64) -> Option<TokenReservation> {
        self.token_budget.reserve(count)
    }

    /// Permanently charge tokens without creating a releasable reservation.
    pub fn try_spend_tokens(&self, count: u64) -> bool {
        self.reserve_tokens(count)
            .map(|mut reservation| {
                reservation.disarm();
                true
            })
            .unwrap_or(false)
    }

    pub fn tokens_available(&self) -> u64 {
        self.token_budget.available()
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

pub struct CallPermit {
    _permit: OwnedSemaphorePermit,
    current_concurrent: Arc<AtomicUsize>,
    token_budget: Arc<TokenBudget>,
    tokens_consumed: u64,
}

impl CallPermit {
    pub fn record_tokens(&mut self, count: u64) {
        self.tokens_consumed = self.tokens_consumed.saturating_add(count);
        if let Some(mut reservation) = self.token_budget.reserve(count) {
            reservation.disarm();
        }
    }
}

impl Drop for CallPermit {
    fn drop(&mut self) {
        self.current_concurrent.fetch_sub(1, Ordering::Relaxed);
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
        let mut reservation = governor.reserve_tokens(7).unwrap();

        assert_eq!(governor.tokens_available(), 3);
        assert!(reservation.reconcile(3));
        assert_eq!(governor.tokens_available(), 7);
        let second_reservation = governor.reserve_tokens(7).unwrap();
        assert!(governor.reserve_tokens(1).is_none());
        drop(second_reservation);
    }

    #[test]
    fn pre_reset_reservation_release_does_not_subtract_new_window_tokens() {
        let governor = ResourceGovernor::new(1, 10);

        let old_reservation = governor.reserve_tokens(7).unwrap();
        governor.token_budget.reset();
        let new_reservation = governor.reserve_tokens(5).unwrap();

        drop(old_reservation);

        assert_eq!(governor.tokens_available(), 5);
        drop(new_reservation);
    }

    #[test]
    fn dropped_token_reservation_releases_tokens() {
        let governor = ResourceGovernor::new(1, 10);
        let reservation = governor.reserve_tokens(7).unwrap();

        assert_eq!(governor.tokens_available(), 3);
        drop(reservation);
        assert_eq!(governor.tokens_available(), 10);
    }

    #[test]
    fn reservation_reconciliation_charges_current_window_after_reset() {
        let governor = ResourceGovernor::new(1, 10);
        let mut old_reservation = governor.reserve_tokens(7).unwrap();

        governor.token_budget.reset();
        let current_reservation = governor.reserve_tokens(2).unwrap();

        assert!(old_reservation.reconcile(5));
        assert_eq!(governor.tokens_available(), 3);

        drop(current_reservation);
    }

    #[test]
    fn reservation_reconciliation_commits_same_window_usage() {
        let governor = ResourceGovernor::new(1, 10);
        let mut under_reservation = governor.reserve_tokens(7).unwrap();

        assert!(under_reservation.reconcile(4));
        assert_eq!(governor.tokens_available(), 6);

        let mut over_reservation = governor.reserve_tokens(2).unwrap();
        assert!(over_reservation.reconcile(6));
        assert_eq!(governor.tokens_available(), 0);
    }

    #[test]
    fn over_budget_reconciliation_keeps_budget_exhausted() {
        let governor = ResourceGovernor::new(1, 10);
        let mut reservation = governor.reserve_tokens(4).unwrap();

        assert!(!reservation.reconcile(20));
        assert_eq!(governor.tokens_available(), 0);
        assert!(governor.reserve_tokens(1).is_none());
    }

    #[test]
    fn reserve_and_reset_are_serialized() {
        let governor = ResourceGovernor::new(1, 10);
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        *governor.token_budget.reserve_hook.lock().unwrap() = Some(Arc::new(ReserveHook {
            entered: entered.clone(),
            release: release.clone(),
        }));

        let reserve_budget = governor.token_budget.clone();
        let reserve_thread = std::thread::spawn(move || reserve_budget.reserve(1).unwrap());
        entered.wait();

        let (reset_started_tx, reset_started_rx) = std::sync::mpsc::channel();
        let reset_finished = Arc::new(AtomicBool::new(false));
        let reset_finished_for_thread = reset_finished.clone();
        let reset_budget = governor.token_budget.clone();
        let reset_thread = std::thread::spawn(move || {
            reset_started_tx.send(()).unwrap();
            reset_budget.reset();
            reset_finished_for_thread.store(true, Ordering::SeqCst);
        });

        reset_started_rx.recv().unwrap();
        std::thread::yield_now();
        assert!(!reset_finished.load(Ordering::SeqCst));

        release.wait();
        let reservation = reserve_thread.join().unwrap();
        reset_thread.join().unwrap();
        assert!(reset_finished.load(Ordering::SeqCst));
        assert_eq!(governor.tokens_available(), 10);
        drop(reservation);
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
