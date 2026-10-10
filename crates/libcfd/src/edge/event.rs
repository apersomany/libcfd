//! One-shot event signals shared across connection attempts.

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

    pub(crate) async fn notified(&self) {
        let notified = self.inner.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_fired() {
            notified.await;
        }
    }
}

impl Clone for Event {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_retains_signal_for_late_waiters() {
        let event = Event::new();
        event.fire();
        tokio::time::timeout(std::time::Duration::from_secs(1), event.notified())
            .await
            .unwrap();
        event.notified().await;
    }
}
