use std::sync::Arc;

use parking_lot::Mutex;

trait AssertionBackend: Send + Sync {
    fn create(&self) -> Result<u32, String>;
    fn release(&self, id: u32) -> Result<(), String>;
}

#[derive(Default)]
struct InhibitorState {
    active_tasks: usize,
    assertion_id: Option<u32>,
}

#[derive(Clone)]
pub struct SleepInhibitor {
    state: Arc<Mutex<InhibitorState>>,
    backend: Arc<dyn AssertionBackend>,
}

pub struct SleepInhibitorGuard {
    inhibitor: Option<SleepInhibitor>,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        Self::with_backend(Arc::new(platform::Backend))
    }

    fn with_backend(backend: Arc<dyn AssertionBackend>) -> Self {
        Self {
            state: Arc::new(Mutex::new(InhibitorState::default())),
            backend,
        }
    }

    pub fn acquire(&self) -> SleepInhibitorGuard {
        let mut state = self.state.lock();

        if state.active_tasks == 0 {
            match self.backend.create() {
                Ok(assertion_id) => state.assertion_id = Some(assertion_id),
                Err(error) => {
                    tracing::warn!(%error, "failed to acquire system sleep inhibitor");
                    return SleepInhibitorGuard { inhibitor: None };
                }
            }
        }

        state.active_tasks += 1;
        SleepInhibitorGuard {
            inhibitor: Some(self.clone()),
        }
    }

    fn release(&self) {
        let assertion_id = {
            let mut state = self.state.lock();
            if state.active_tasks == 0 {
                return;
            }

            state.active_tasks -= 1;
            if state.active_tasks == 0 {
                state.assertion_id.take()
            } else {
                None
            }
        };

        if let Some(assertion_id) = assertion_id {
            if let Err(error) = self.backend.release(assertion_id) {
                tracing::warn!(%error, "failed to release system sleep inhibitor");
            }
        }
    }
}

impl Default for SleepInhibitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SleepInhibitorGuard {
    fn drop(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.release();
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::AssertionBackend;
    use core_foundation::{
        base::TCFType,
        string::{CFString, CFStringRef},
    };

    const ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    pub struct Backend;

    impl AssertionBackend for Backend {
        fn create(&self) -> Result<u32, String> {
            let assertion_type = CFString::new("PreventUserIdleSystemSleep");
            let assertion_name = CFString::new("Averroes user-started agent task");
            let mut assertion_id = 0;
            let return_code = unsafe {
                IOPMAssertionCreateWithName(
                    assertion_type.as_concrete_TypeRef(),
                    ASSERTION_LEVEL_ON,
                    assertion_name.as_concrete_TypeRef(),
                    &mut assertion_id,
                )
            };

            if return_code == 0 {
                Ok(assertion_id)
            } else {
                Err(format!(
                    "IOPMAssertionCreateWithName returned {return_code}"
                ))
            }
        }

        fn release(&self, id: u32) -> Result<(), String> {
            let return_code = unsafe { IOPMAssertionRelease(id) };
            if return_code == 0 {
                Ok(())
            } else {
                Err(format!("IOPMAssertionRelease returned {return_code}"))
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::AssertionBackend;

    pub struct Backend;

    impl AssertionBackend for Backend {
        fn create(&self) -> Result<u32, String> {
            Ok(0)
        }

        fn release(&self, _id: u32) -> Result<(), String> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    struct RecordingBackend {
        create_calls: AtomicUsize,
        next_id: AtomicU32,
        release_calls: parking_lot::Mutex<Vec<u32>>,
        fail_creation: AtomicBool,
    }

    impl RecordingBackend {
        fn created(&self) -> usize {
            self.create_calls.load(Ordering::SeqCst)
        }

        fn released(&self) -> Vec<u32> {
            self.release_calls.lock().clone()
        }
    }

    impl AssertionBackend for RecordingBackend {
        fn create(&self) -> Result<u32, String> {
            self.create_calls.fetch_add(1, Ordering::SeqCst);
            let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_creation.load(Ordering::SeqCst) {
                return Err("simulated assertion failure".into());
            }
            Ok(id)
        }

        fn release(&self, id: u32) -> Result<(), String> {
            self.release_calls.lock().push(id);
            Ok(())
        }
    }

    fn inhibitor(backend: Arc<RecordingBackend>) -> SleepInhibitor {
        SleepInhibitor::with_backend(backend)
    }

    #[test]
    fn first_guard_creates_one_assertion_and_last_guard_releases_it() {
        let backend = Arc::new(RecordingBackend::default());
        let inhibitor = inhibitor(backend.clone());

        let guard = inhibitor.acquire();
        assert_eq!(backend.created(), 1);
        assert!(backend.released().is_empty());

        drop(guard);
        assert_eq!(backend.released(), vec![1]);
    }

    #[test]
    fn concurrent_guards_share_one_assertion_until_the_last_guard_drops() {
        let backend = Arc::new(RecordingBackend::default());
        let inhibitor = inhibitor(backend.clone());

        let first = inhibitor.acquire();
        let second = inhibitor.acquire();
        assert_eq!(backend.created(), 1);

        drop(first);
        assert!(backend.released().is_empty());

        drop(second);
        assert_eq!(backend.released(), vec![1]);
    }

    #[test]
    fn failed_creation_returns_a_noop_guard_and_allows_a_later_retry() {
        let backend = Arc::new(RecordingBackend::default());
        backend.fail_creation.store(true, Ordering::SeqCst);
        let inhibitor = inhibitor(backend.clone());

        let failed = inhibitor.acquire();
        drop(failed);
        assert_eq!(backend.created(), 1);
        assert!(backend.released().is_empty());

        backend.fail_creation.store(false, Ordering::SeqCst);
        let retry = inhibitor.acquire();
        assert_eq!(backend.created(), 2);
        drop(retry);
        assert_eq!(backend.released(), vec![2]);
    }
}
