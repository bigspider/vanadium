//! Runtime helpers for driving a V-App from a web page (`target_wasm`, architecture A).
//!
//! A V-App's wasm module re-exports these (with `#[no_mangle]`) so the page can paint the
//! framebuffer to a `<canvas>`. The framebuffer is the same one the display ECALLs draw
//! into; `App::dispatch_blocking` runs a command, the display ops update it, and the page
//! reads it here.

/// Pointer to the framebuffer (one byte per pixel, intensity 0..=15), in wasm memory.
/// Stable for the life of the module.
pub fn framebuffer_ptr() -> *const u8 {
    crate::ecalls_wasm::framebuffer_ptr()
}

/// Framebuffer width in pixels.
pub fn framebuffer_width() -> usize {
    crate::ecalls_wasm::framebuffer_dims().0
}

/// Framebuffer height in pixels.
pub fn framebuffer_height() -> usize {
    crate::ecalls_wasm::framebuffer_dims().1
}

/// A counter bumped on every `display_refresh`, so the page can repaint only on change.
pub fn framebuffer_version() -> u64 {
    crate::ecalls_wasm::framebuffer_version()
}

// ---------------------------------------------------------------------------
// Step driver — runs a V-App command that may await user input, one poll at a
// time, so JS keeps the event loop (architecture A). `start` builds the handler
// future; `poll` advances it; when the handler awaits `get_event` with no input
// queued it suspends, `poll` returns `Pending`, and the page renders the frame
// and feeds input (via `push_touch` / `push_button` / `push_quit`) before polling
// again.
// ---------------------------------------------------------------------------

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::app::{App, AppBuilder};

/// Drives one V-App across JS poll calls. Holds the app and the in-flight command, plus
/// the handler future that borrows them.
pub struct WasmDriver<S = ()> {
    // `app` and `cmd` live in stable heap allocations and outlive `fut`, which borrows
    // them; we erase those borrows to `'static` and uphold soundness by construction (see
    // the SAFETY note in `start`): single-threaded, and `app`/`cmd` are never touched or
    // replaced while `fut` is `Some`.
    app: Box<App<S>>,
    cmd: Box<[u8]>,
    fut: Option<Pin<Box<dyn Future<Output = Vec<u8>>>>>,
}

impl<S: Default + 'static> WasmDriver<S> {
    /// Builds the driver from an `AppBuilder` (no command in flight yet).
    pub fn new(builder: AppBuilder<S>) -> Self {
        Self {
            app: Box::new(builder.build_wasm()),
            cmd: Box::new([]),
            fut: None,
        }
    }

    /// Whether a command is currently being handled.
    pub fn busy(&self) -> bool {
        self.fut.is_some()
    }

    /// Begins handling `cmd`. Panics if a command is already in flight.
    pub fn start(&mut self, cmd: Vec<u8>) {
        assert!(!self.busy(), "a command is already in flight");
        self.cmd = cmd.into_boxed_slice();
        let handler = self.app.handler;
        let app_ptr: *mut App<S> = &mut *self.app;
        let cmd_ptr: *const [u8] = &*self.cmd;
        // SAFETY: `app`/`cmd` are boxed (stable addresses) and owned by `self`, so they
        // live at least as long as `fut`. We never touch or replace them while `fut` is
        // `Some` (`start` asserts `!busy()`, and `framebuffer`/other access does not reach
        // them), and wasm is single-threaded — so the future never sees a dangling or
        // aliased reference. The `'static` erasure is what the borrow checker cannot prove.
        self.fut = Some(handler(unsafe { &mut *app_ptr }, unsafe { &*cmd_ptr }));
    }

    /// Advances the in-flight command. `Ready(response)` when it finishes (the driver is
    /// idle again), `Pending` when it is waiting for input. Returns `Pending` when idle.
    pub fn poll(&mut self) -> Poll<Vec<u8>> {
        let Some(fut) = self.fut.as_mut() else {
            return Poll::Pending;
        };
        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(resp) => {
                self.fut = None;
                Poll::Ready(resp)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Queues a touch event for the running handler.
pub fn push_touch(x: u16, y: u16, pressed: bool) {
    use common::ux::{EventCode, EventData, TouchEvent, TouchState};
    let mut ed = EventData::default();
    ed.touch = TouchEvent::new(
        x,
        y,
        if pressed {
            TouchState::Pressed
        } else {
            TouchState::Released
        },
    );
    crate::ecalls_wasm::push_event(EventCode::Touch, ed);
}

/// Queues an `Action::Quit` so a handler's event loop can end.
pub fn push_quit() {
    use common::ux::{Action, EventCode, EventData};
    let mut ed = EventData::default();
    ed.action = Action::Quit;
    crate::ecalls_wasm::push_event(EventCode::Action, ed);
}

/// Queues a `Ticker` event — the device clock. Pump this on a timer while the app is idle so
/// `App::idle_ux_step` can run the dashboard's timers (e.g. the return-to-dashboard countdown
/// a timed `show_info` schedules).
pub fn push_ticker() {
    use common::ux::{EventCode, EventData};
    crate::ecalls_wasm::push_event(EventCode::Ticker, EventData::default());
}
