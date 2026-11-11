use crate::{Error, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

pub(crate) const ACTIVE_SLOTS: usize = 4;
pub(crate) const QUEUED_SLOTS: usize = 32;
const SLOT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct Admission {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    active: usize,
    queued: usize,
    waiting: VecDeque<u64>,
    next_ticket: u64,
    closing: bool,
}

pub(crate) struct Permit {
    owner: Arc<Admission>,
    slots: usize,
}

pub(crate) fn request_slots(body_bytes: usize) -> usize {
    body_bytes.div_ceil(SLOT_BYTES).clamp(1, ACTIVE_SLOTS)
}

impl Admission {
    pub fn acquire(self: &Arc<Self>, deadline: Instant, slots: usize) -> Result<Permit> {
        assert!((1..=ACTIVE_SLOTS).contains(&slots));
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::new("RDB_STORAGE", "admission state failed"))?;
        if state.closing {
            return Err(Error::new("RDB_CLOSED", "service is stopping"));
        }
        if !state.waiting.is_empty() || state.active + slots > ACTIVE_SLOTS {
            state = self.wait_in_line(state, deadline, slots)?;
        }
        if Instant::now() >= deadline {
            return Err(Error::new("RDB_CANCELLED", "request deadline expired"));
        }
        state.active += slots;
        Ok(Permit {
            owner: self.clone(),
            slots,
        })
    }

    fn wait_in_line<'a>(
        &'a self,
        mut state: MutexGuard<'a, State>,
        deadline: Instant,
        slots: usize,
    ) -> Result<MutexGuard<'a, State>> {
        if state.queued + slots > QUEUED_SLOTS {
            return Err(Error::new("RDB_BUSY", "service queue is full"));
        }
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.waiting.push_back(ticket);
        state.queued += slots;
        loop {
            if state.closing {
                self.leave_line(&mut state, ticket, slots);
                return Err(Error::new("RDB_CLOSED", "service is stopping"));
            }
            if state.waiting.front() == Some(&ticket) && state.active + slots <= ACTIVE_SLOTS {
                self.leave_line(&mut state, ticket, slots);
                return Ok(state);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.leave_line(&mut state, ticket, slots);
                return Err(Error::new(
                    "RDB_CANCELLED",
                    "request deadline expired in queue",
                ));
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| Error::new("RDB_STORAGE", "admission wait failed"))?
                .0;
        }
    }

    fn leave_line(&self, state: &mut State, ticket: u64, slots: usize) {
        let position = state
            .waiting
            .iter()
            .position(|waiting| *waiting == ticket)
            .expect("a waiting request keeps its place in line");
        state.waiting.remove(position);
        state.queued -= slots;
        self.changed.notify_all();
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
            state.active -= self.slots;
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
                    .acquire(Instant::now() + Duration::from_secs(5), 1)
                    .unwrap()
            })
            .collect();
        let waiting: Vec<_> = (0..32)
            .map(|_| {
                let admission = admission.clone();
                std::thread::spawn(move || {
                    admission
                        .acquire(Instant::now() + Duration::from_secs(5), 1)
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
                .acquire(Instant::now() + Duration::from_secs(1), 1)
                .err()
                .unwrap()
                .code,
            "RDB_BUSY"
        );
        admission.stop();
        for thread in waiting {
            assert_eq!(thread.join().unwrap(), "RDB_CLOSED");
        }
        assert_eq!(admission.counts(), (4, 0));
        drop(active);
        assert_eq!(admission.counts(), (0, 0));
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_secs(1), 1)
                .err()
                .unwrap()
                .code,
            "RDB_CLOSED"
        );
    }

    #[test]
    fn expired_waits_release_queue_capacity_without_stopping_the_owner() {
        let admission = Arc::new(Admission::default());
        let active: Vec<_> = (0..4)
            .map(|_| {
                admission
                    .acquire(Instant::now() + Duration::from_secs(1), 1)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_millis(20), 1)
                .err()
                .unwrap()
                .code,
            "RDB_CANCELLED"
        );
        assert_eq!(admission.counts(), (4, 0));
        drop(active);
        assert!(admission
            .acquire(Instant::now() + Duration::from_secs(1), 1)
            .is_ok());
    }

    #[test]
    fn large_requests_take_more_slots_and_later_requests_wait_behind_them() {
        assert_eq!(request_slots(0), 1);
        assert_eq!(request_slots(SLOT_BYTES), 1);
        assert_eq!(request_slots(SLOT_BYTES + 1), 2);
        assert_eq!(request_slots(3 * SLOT_BYTES), 3);
        assert_eq!(request_slots(8 * SLOT_BYTES), ACTIVE_SLOTS);
        let admission = Arc::new(Admission::default());
        let small = admission
            .acquire(Instant::now() + Duration::from_secs(5), 1)
            .unwrap();
        let large = {
            let admission = admission.clone();
            std::thread::spawn(move || {
                admission
                    .acquire(Instant::now() + Duration::from_secs(5), ACTIVE_SLOTS)
                    .map(|_| ())
            })
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while admission.counts().1 < ACTIVE_SLOTS {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(admission.counts(), (1, ACTIVE_SLOTS));
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_millis(50), 1)
                .err()
                .unwrap()
                .code,
            "RDB_CANCELLED"
        );
        assert_eq!(admission.counts(), (1, ACTIVE_SLOTS));
        drop(small);
        large.join().unwrap().unwrap();
        assert_eq!(admission.counts(), (0, 0));
        let full: Vec<_> = (0..ACTIVE_SLOTS)
            .map(|_| {
                admission
                    .acquire(Instant::now() + Duration::from_secs(5), 1)
                    .unwrap()
            })
            .collect();
        let waiting: Vec<_> = (0..QUEUED_SLOTS / ACTIVE_SLOTS)
            .map(|_| {
                let admission = admission.clone();
                std::thread::spawn(move || {
                    admission
                        .acquire(Instant::now() + Duration::from_secs(5), ACTIVE_SLOTS)
                        .err()
                        .unwrap()
                        .code
                })
            })
            .collect();
        while admission.counts().1 < QUEUED_SLOTS {
            assert!(Instant::now() < deadline + Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            admission
                .acquire(Instant::now() + Duration::from_secs(1), 1)
                .err()
                .unwrap()
                .code,
            "RDB_BUSY"
        );
        admission.stop();
        for thread in waiting {
            assert_eq!(thread.join().unwrap(), "RDB_CLOSED");
        }
        drop(full);
        assert_eq!(admission.counts(), (0, 0));
    }
}
