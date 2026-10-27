use crate::{Error, Result};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

#[derive(Default)]
pub(crate) struct Admission {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    active: usize,
    queued: usize,
    closing: bool,
}

pub(crate) struct Permit {
    owner: Arc<Admission>,
}

impl Admission {
    pub fn acquire(self: &Arc<Self>, deadline: Instant) -> Result<Permit> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::new("FDB_STORAGE", "admission state failed"))?;
        if state.closing {
            return Err(Error::new("FDB_CLOSED", "service is stopping"));
        }
        if state.active >= 4 {
            if state.queued >= 32 {
                return Err(Error::new("FDB_BUSY", "service queue is full"));
            }
            state.queued += 1;
            while state.active >= 4 && !state.closing {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    state.queued -= 1;
                    return Err(Error::new(
                        "FDB_CANCELLED",
                        "request deadline expired in queue",
                    ));
                }
                let waited = self
                    .changed
                    .wait_timeout(state, remaining)
                    .map_err(|_| Error::new("FDB_STORAGE", "admission wait failed"))?;
                state = waited.0;
            }
            state.queued -= 1;
        }
        if state.closing {
            return Err(Error::new("FDB_CLOSED", "service is stopping"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new("FDB_CANCELLED", "request deadline expired"));
        }
        state.active += 1;
        Ok(Permit {
            owner: self.clone(),
        })
    }

    pub fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closing = true;
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.active, state.queued)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.owner.state.lock() {
            state.active -= 1;
            self.owner.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn admission_bounds_active_requests_and_queue_and_wakes_on_shutdown() {
        let admission = Arc::new(Admission::default());
        let active: Vec<_> = (0..4)
            .map(|_| {
                admission
                    .acquire(Instant::now() + Duration::from_secs(5))
                    .unwrap()
            })
            .collect();
        let waiting: Vec<_> = (0..32)
            .map(|_| {
                let admission = admission.clone();
                std::thread::spawn(move || {
                    admission
                        .acquire(Instant::now() + Duration::from_secs(5))
                        .err()
                        .unwrap()
                        .code
                })
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        while admission.counts().1 < 32 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(admission.counts(), (4, 32));
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_secs(1))
                .err()
                .unwrap()
                .code,
            "FDB_BUSY"
        );
        admission.stop();
        for thread in waiting {
            assert_eq!(thread.join().unwrap(), "FDB_CLOSED");
        }
        assert_eq!(admission.counts(), (4, 0));
        drop(active);
        assert_eq!(admission.counts(), (0, 0));
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_secs(1))
                .err()
                .unwrap()
                .code,
            "FDB_CLOSED"
        );
    }

    #[test]
    fn expired_waits_release_queue_capacity_without_stopping_the_owner() {
        let admission = Arc::new(Admission::default());
        let active: Vec<_> = (0..4)
            .map(|_| {
                admission
                    .acquire(Instant::now() + Duration::from_secs(1))
                    .unwrap()
            })
            .collect();
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_millis(20))
                .err()
                .unwrap()
                .code,
            "FDB_CANCELLED"
        );
        assert_eq!(admission.counts(), (4, 0));
        drop(active);
        assert!(admission
            .acquire(Instant::now() + Duration::from_secs(1))
            .is_ok());
    }
}
