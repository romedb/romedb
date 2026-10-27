//! Deliver terminal SIGINT outside the async-signal context.
use signal_hook::{
    consts::SIGINT,
    iterator::{Handle, Signals},
};
use std::{
    io,
    thread::{self, JoinHandle},
};

pub struct Interrupts {
    handle: Handle,
    worker: Option<JoinHandle<()>>,
}
impl Interrupts {
    pub fn new(interrupt: romedb::InterruptHandle) -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT])?;
        let handle = signals.handle();
        let worker = thread::Builder::new()
            .name("romedb-cli-interrupt".into())
            .spawn(move || {
                for _ in signals.forever() {
                    // Weak handles do not own the connection. Engine interrupt
                    // requests while idle do not poison the next statement.
                    if !interrupt.interrupt() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            handle,
            worker: Some(worker),
        })
    }
}
impl Drop for Interrupts {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(worker) = self.worker.take() {
            // The worker performs no fallible work or application callbacks.
            worker.join().expect("CLI interrupt worker panicked");
        }
    }
}
