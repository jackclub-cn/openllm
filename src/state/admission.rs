use super::*;

/// Global budget on the total bytes of in-flight request bodies.
///
/// The gateway buffers each request body in memory, so the number of requests
/// is a poor proxy for memory pressure: a handful of large coding-agent
/// payloads can use more RAM than hundreds of small ones. This budget charges
/// the actual bytes of every admitted body and releases them once the response
/// finishes, so the process can shed over-budget requests at admission instead
/// of buffering past its memory headroom.
pub struct RequestByteBudget {
    pub(super) limit: usize,
    pub(super) used: AtomicUsize,
    /// Woken whenever a reservation is released so a bounded wait can retry.
    pub(super) released: tokio::sync::Notify,
}

impl RequestByteBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
            released: tokio::sync::Notify::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Bytes currently reserved by admitted requests.
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// Headroom left in the budget, saturating at zero.
    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used())
    }

    /// Adds `bytes` when the budget still has room, returning `false` otherwise.
    ///
    /// The compare-and-swap loop keeps concurrent admissions from racing past
    /// the limit; a zero-length add always succeeds so empty bodies never trip
    /// the guard.
    pub(super) fn try_add(&self, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > self.limit {
                return false;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    pub(super) fn release(&self, bytes: usize) {
        if bytes > 0 {
            self.used.fetch_sub(bytes, Ordering::AcqRel);
            self.released.notify_waiters();
        }
    }

    /// Reserves `bytes` against the budget, returning a guard that releases
    /// them on drop. Returns `None` when the reservation would exceed the limit.
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<RequestByteGuard> {
        let mut guard = RequestByteGuard {
            budget: Arc::clone(self),
            reserved: 0,
        };
        guard.extend(bytes).then_some(guard)
    }

    /// Reserves `bytes`, waiting up to `wait` for in-flight requests to release
    /// room before giving up. A zero `wait` is a non-blocking [`Self::reserve`].
    pub async fn reserve_within(
        self: &Arc<Self>,
        bytes: usize,
        wait: Duration,
    ) -> Option<RequestByteGuard> {
        if wait.is_zero() {
            return self.reserve(bytes);
        }
        let deadline = Instant::now() + wait;
        loop {
            // Register the waiter before checking so a release that lands
            // between a failed reservation and the await is not missed.
            let released = self.released.notified();
            if let Some(guard) = self.reserve(bytes) {
                return Some(guard);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            if tokio::time::timeout(remaining, released).await.is_err() {
                // Timed out; a release may still have landed, so try once more.
                return self.reserve(bytes);
            }
        }
    }
}

/// Holds reserved bytes against a [`RequestByteBudget`] until it is dropped.
///
/// A single guard accumulates every charge for one request, so it can be moved
/// into the response body and released only once the whole request (body read,
/// upstream work, and any streamed response) has finished.
pub struct RequestByteGuard {
    pub(super) budget: Arc<RequestByteBudget>,
    pub(super) reserved: usize,
}

impl RequestByteGuard {
    /// Bytes currently reserved by this guard.
    pub fn reserved(&self) -> usize {
        self.reserved
    }

    /// Charges `bytes` more against the budget, returning `false` when the
    /// request would exceed the limit (leaving the reservation unchanged).
    pub fn extend(&mut self, bytes: usize) -> bool {
        if self.budget.try_add(bytes) {
            self.reserved += bytes;
            true
        } else {
            false
        }
    }

    /// Releases any reservation above `bytes`, keeping the accounting exact
    /// when a declared `Content-Length` overstates the actual body size.
    pub fn shrink_to(&mut self, bytes: usize) {
        if bytes < self.reserved {
            self.budget.release(self.reserved - bytes);
            self.reserved = bytes;
        }
    }
}

impl Drop for RequestByteGuard {
    fn drop(&mut self) {
        self.budget.release(self.reserved);
    }
}
