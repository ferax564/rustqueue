//! Background housekeeping lifecycle for embedded `RustQueue`.
//!
//! Holds the scheduler `JoinHandle` and aborts it when the last `RustQueue`
//! clone is dropped. There is no public Drop-guard handle to forget.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use tokio::task::JoinHandle;

pub(crate) struct HousekeepingState {
    pub(crate) started: AtomicBool,
    pub(crate) task: Mutex<Option<JoinHandle<()>>>,
}

impl HousekeepingState {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            task: Mutex::new(None),
        }
    }
}

impl Drop for HousekeepingState {
    fn drop(&mut self) {
        if let Some(handle) = self.task.lock().unwrap().take() {
            handle.abort();
        }
    }
}
