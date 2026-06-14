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

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::app::App;

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

// ===========================================================================
// Global device — the installed V-App, shared by the co-resident client (which
// drives it with commands) and the page (which pumps the dashboard between
// commands). One app per wasm module (architecture A). This is what lets the
// generic device shell below stay app-independent while a real Rust client,
// compiled to JS via wasm-bindgen, talks to the same app.
// ===========================================================================

use core::cell::{Cell, RefCell};
use wasm_bindgen::prelude::*;

thread_local! {
    static DEVICE: RefCell<Option<Box<App>>> = const { RefCell::new(None) };
    // >0 while a client command is in flight, so the idle pump leaves its screen alone.
    static COMMAND_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Installs the V-App to run in the page. Call once at startup, before any command or tick.
pub fn install(app: App) {
    DEVICE.with(|d| *d.borrow_mut() = Some(Box::new(app)));
}

// A stable raw pointer to the installed app. The borrow is released immediately; callers
// drive the app through the pointer. Sound because wasm is single-threaded, the app is boxed
// (stable) and owned by DEVICE for the module's life, and commands never overlap the idle
// pump (the shell's `tick`/`idle_touch` no-op while a command is in flight).
fn app_ptr() -> *mut App {
    DEVICE.with(|d| {
        let mut g = d.borrow_mut();
        &mut **g
            .as_mut()
            .expect("wasm_runtime::install must be called first") as *mut App
    })
}

/// Drives the installed app's handler for one message and returns its response. Suspends as a
/// real future if the handler awaits on-device input (the page feeds input via the shell, and
/// the awaiting client resolves as a JS Promise). Used by the co-resident client transport.
pub async fn dispatch(msg: &[u8]) -> Vec<u8> {
    let p = app_ptr();
    // SAFETY: see `app_ptr`. The returned future borrows the app for its lifetime; nothing
    // else touches the app while a command is in flight.
    unsafe { (*p).dispatch(msg).await }
}

/// RAII marker that a client command is in flight, so the page's idle pump doesn't draw the
/// dashboard over the command's screen. Held across the command's suspensions; `Drop` clears
/// it even if the command future is dropped/cancelled.
pub struct CommandGuard(());

impl CommandGuard {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        COMMAND_DEPTH.with(|c| c.set(c.get() + 1));
        CommandGuard(())
    }
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        COMMAND_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

fn command_active() -> bool {
    COMMAND_DEPTH.with(|c| c.get() > 0)
}

// One idle/dashboard step. The caller queues exactly one event first (a ticker or a touch),
// so `idle_ux_step` consumes it and returns without parking.
fn idle_step() {
    let p = app_ptr();
    // SAFETY: see `app_ptr`; only runs while no command is in flight.
    crate::executor::block_on(unsafe { (*p).idle_ux_step() });
}

// ===========================================================================
// Generic device shell, exposed to JS via wasm-bindgen. App-independent: any
// V-App gets these for free; only the command bindings are app-specific.
// ===========================================================================

/// Pointer to the Gray4 framebuffer (one byte per pixel, 0..=15) in wasm memory.
#[wasm_bindgen(js_name = vappFbPtr)]
pub fn js_fb_ptr() -> usize {
    framebuffer_ptr() as usize
}
#[wasm_bindgen(js_name = vappFbWidth)]
pub fn js_fb_width() -> usize {
    framebuffer_width()
}
#[wasm_bindgen(js_name = vappFbHeight)]
pub fn js_fb_height() -> usize {
    framebuffer_height()
}
#[wasm_bindgen(js_name = vappFbVersion)]
pub fn js_fb_version() -> f64 {
    framebuffer_version() as f64
}

/// Queues a touch for a running command's screen (press then release maps to two calls).
#[wasm_bindgen(js_name = vappPushTouch)]
pub fn js_push_touch(x: u32, y: u32, pressed: bool) {
    push_touch(x as u16, y as u16, pressed);
}

/// Queues an `Action::Quit` for a running command's event loop.
#[wasm_bindgen(js_name = vappPushQuit)]
pub fn js_push_quit() {
    push_quit();
}

/// Pumps one tick of the device clock while idle: draws the dashboard and advances its timers
/// (e.g. the return-to-dashboard countdown after a timed result screen). No-op while a client
/// command is in flight. The page calls this on a ~100 ms timer.
#[wasm_bindgen(js_name = vappTick)]
pub fn js_tick() {
    if command_active() {
        return;
    }
    push_ticker();
    idle_step();
}

/// Feeds a dashboard tap (press + release) while idle — drives its app-info / quit navigation.
/// No-op while a command is in flight (command screens take taps via `vappPushTouch`).
#[wasm_bindgen(js_name = vappIdleTouch)]
pub fn js_idle_touch(x: u32, y: u32) {
    if command_active() {
        return;
    }
    push_touch(x as u16, y as u16, true);
    idle_step();
    push_touch(x as u16, y as u16, false);
    idle_step();
}
