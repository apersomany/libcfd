//! One-shot event signals shared across connection attempts.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// A one-shot event: any task can fire it, any task can wait on it.
///
/// Used for the shutdown signal that ends a tunnel run and for signaling
/// that registration completed on a control stream.
pub(crate) struct Event {
    inner: Arc<EventInner>,
}

struct EventInner {
    fired: AtomicBool,
    notify: Notify,
}

impl Event {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(EventInner {
                fired: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    pub(crate) fn fire(&self) {
        self.inner.fired.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub(crate) fn is_fired(&self) -> bool {
        self.inner.fired.load(Ordering::SeqCst)
    }

    pub(crate) fn notified(&self) -> impl Future<Output = ()> + Send + '_ {
        self.inner.notify.notified()
    }
}

impl Clone for Event {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
