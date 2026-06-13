//! Interactive V-App compiled to WebAssembly, validating architecture A end to end: a
//! handler that draws and **awaits user input** (a tap counter) runs inside wasm, driven
//! one poll at a time from JS via the SDK's `WasmDriver` step driver.
//!
//! Hand-rolled ABI (no wasm-bindgen):
//!   * `app_init()` builds the driver once.
//!   * `app_start(len)` begins handling a command (`len` bytes from `IO`).
//!   * `app_poll()` advances it: returns -1 while waiting for input (the page paints the
//!     framebuffer and feeds input), else the response length (written to `IO`).
//!   * `app_push_touch(x,y,pressed)` / `app_push_quit()` queue input events.
//!   * `fb_*` expose the framebuffer for the page to paint to a <canvas>.

extern crate alloc;

use core::ptr::addr_of_mut;
use core::task::Poll;
use std::cell::RefCell;

use sdk::ux::{Action, Event, TouchState};
use sdk::wasm_runtime::WasmDriver;
use sdk::{App, AppBuilder};

const IO_CAP: usize = 16 * 1024;
static mut IO: [u8; IO_CAP] = [0; IO_CAP];

thread_local! {
    static DRIVER: RefCell<Option<WasmDriver>> = const { RefCell::new(None) };
}

/// The interactive handler: draw a tap counter and update it on each touch until quit.
#[sdk::handler]
async fn process(_app: &mut App, _msg: &[u8]) -> Vec<u8> {
    let mut count: u32 = 0;
    draw(count);
    loop {
        match sdk::ux::get_event().await {
            Event::Touch(te) if te.state == TouchState::Pressed => {
                count += 1;
                draw(count);
            }
            Event::Action(Action::Quit) => break,
            _ => {}
        }
    }
    alloc::format!("count={count}").into_bytes()
}

/// Draws the counter screen with the SDK's drawing primitives (the same path on device).
fn draw(count: u32) {
    use sdk::ui::{Align, Font, Rect, Scene, Surface};
    let mut surf = Surface::new();
    let screen = surf.screen();
    let theme = surf.theme();
    let mut sc = Scene::new();
    sc.rect(screen, theme.bg);
    sc.text(
        Rect::new(0, screen.h / 2 - 30, screen.w, 28),
        alloc::format!("Taps: {count}"),
        Font::Large,
        theme.fg,
        theme.bg,
        Align::Center,
    );
    sc.text(
        Rect::new(0, screen.h / 2 + 10, screen.w, 20),
        "tap anywhere",
        Font::Regular,
        theme.fg,
        theme.bg,
        Align::Center,
    );
    surf.paint(&sc);
}

#[no_mangle]
pub extern "C" fn io_ptr() -> *mut u8 {
    addr_of_mut!(IO) as *mut u8
}
#[no_mangle]
pub extern "C" fn io_cap() -> usize {
    IO_CAP
}

#[no_mangle]
pub extern "C" fn app_init() {
    DRIVER.with(|d| {
        *d.borrow_mut() = Some(WasmDriver::new(AppBuilder::new(
            "ui-demo",
            env!("CARGO_PKG_VERSION"),
            process,
        )));
    });
}

#[no_mangle]
pub extern "C" fn app_start(len: usize) {
    let io = addr_of_mut!(IO) as *const u8;
    // SAFETY: single-threaded wasm; JS only touches IO between our calls.
    let cmd = unsafe { std::slice::from_raw_parts(io, len.min(IO_CAP)) }.to_vec();
    DRIVER.with(|d| d.borrow_mut().as_mut().expect("app_init").start(cmd));
}

/// Advances the in-flight command: -1 while it waits for input, else the response length
/// (written into `IO`).
#[no_mangle]
pub extern "C" fn app_poll() -> i64 {
    DRIVER.with(|d| match d.borrow_mut().as_mut().expect("app_init").poll() {
        Poll::Ready(resp) => {
            let n = resp.len().min(IO_CAP);
            let io = addr_of_mut!(IO) as *mut u8;
            unsafe { std::slice::from_raw_parts_mut(io, n) }.copy_from_slice(&resp[..n]);
            n as i64
        }
        Poll::Pending => -1,
    })
}

#[no_mangle]
pub extern "C" fn app_push_touch(x: u32, y: u32, pressed: u32) {
    sdk::wasm_runtime::push_touch(x as u16, y as u16, pressed != 0);
}

#[no_mangle]
pub extern "C" fn app_push_quit() {
    sdk::wasm_runtime::push_quit();
}

// --- framebuffer accessors for the page (paints it to a <canvas>) ---
#[no_mangle]
pub extern "C" fn fb_ptr() -> *const u8 {
    sdk::wasm_runtime::framebuffer_ptr()
}
#[no_mangle]
pub extern "C" fn fb_width() -> usize {
    sdk::wasm_runtime::framebuffer_width()
}
#[no_mangle]
pub extern "C" fn fb_height() -> usize {
    sdk::wasm_runtime::framebuffer_height()
}
#[no_mangle]
pub extern "C" fn fb_version() -> u64 {
    sdk::wasm_runtime::framebuffer_version()
}
