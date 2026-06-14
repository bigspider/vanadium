//! Cooperative step driver for a co-resident client (architecture A, wasm32).
//!
//! A client command (e.g. `BitcoinClient::get_extended_pubkey` with `display`) can drive the
//! co-resident V-App into an on-device UI flow that **awaits user input**. Polling the whole
//! client-command future one step at a time — instead of `block_on`-ing it — lets that
//! suspension propagate all the way back to JS, which paints the framebuffer and feeds input
//! (touch / quit) before polling again.
//!
//! This mirrors app-sdk's `WasmDriver`, but holds a *client* and an in-flight command future
//! that borrows it. The command future resolves to a `Vec<u8>` so the page gets a uniform
//! result channel regardless of the command's native return type.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

/// Drives one co-resident client across JS poll calls. Holds the client in a stable heap
/// allocation and the in-flight command future that borrows it.
pub struct WasmClientDriver<C> {
    // `client` lives in a stable boxed allocation that outlives `fut`, which borrows it; the
    // borrow is erased to `'static` and kept sound by construction (see `start`): wasm is
    // single-threaded, and `client` is never moved or touched while `fut` is `Some`.
    client: Box<C>,
    fut: Option<Pin<Box<dyn Future<Output = Vec<u8>>>>>,
}

impl<C: 'static> WasmClientDriver<C> {
    /// Wraps a client (no command in flight yet).
    pub fn new(client: C) -> Self {
        Self {
            client: Box::new(client),
            fut: None,
        }
    }

    /// Whether a command is currently being driven.
    pub fn busy(&self) -> bool {
        self.fut.is_some()
    }

    /// Begins a command. `build` receives a mutable borrow of the client and returns the
    /// command future — typically `Box::pin(async move { ... })` that calls a client method
    /// and encodes its result into the returned bytes. Panics if a command is already in
    /// flight.
    pub fn start<F>(&mut self, build: F)
    where
        F: FnOnce(&'static mut C) -> Pin<Box<dyn Future<Output = Vec<u8>> + 'static>>,
    {
        assert!(!self.busy(), "a command is already in flight");
        let client_ptr: *mut C = &mut *self.client;
        // SAFETY: `client` is boxed (stable address) and owned by `self`, so it outlives
        // `fut`. While `fut` is `Some` we never move or otherwise touch `client` (`start`
        // asserts `!busy()`), and wasm is single-threaded, so the future never observes a
        // dangling or aliased borrow. The `'static` erasure is what the borrow checker cannot
        // prove.
        self.fut = Some(build(unsafe { &mut *client_ptr }));
    }

    /// Advances the in-flight command: `Ready(bytes)` when it finishes (the driver is idle
    /// again), `Pending` while it waits for on-device input. Returns `Pending` when idle.
    pub fn poll(&mut self) -> Poll<Vec<u8>> {
        let Some(fut) = self.fut.as_mut() else {
            return Poll::Pending;
        };
        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => {
                self.fut = None;
                Poll::Ready(out)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
