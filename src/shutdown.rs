//! A shutdown signal shared across connection attempts.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// A one-shot shutdown signal: any task can fire it, any task can wait on it.
pub(crate) struct Shutdown {
    fired: AtomicBool,
    notify: Notify,
}

impl Shutdown {
    pub(crate) fn new() -> Arc<Shutdown> {
        Arc::new(Shutdown {
            fired: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub(crate) fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    pub(crate) fn notified(&self) -> impl Future<Output = ()> + Send + '_ {
        self.notify.notified()
    }
}
