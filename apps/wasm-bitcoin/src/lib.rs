//! The real Bitcoin V-App running in a web page, driven by the **real** `BitcoinClient`
//! (the same client the CLI uses) — both co-resident in one wasm module, no server, no
//! device (architecture A).
//!
//! Two command kinds are exposed, both driven by the cooperative `WasmClientDriver` so JS
//! keeps the event loop:
//!   * `bitcoin_start_get_fingerprint()` — request/response, completes in one poll.
//!   * `bitcoin_start_get_pubkey(display)` — `GetExtendedPubkey`; with `display=1` the app
//!     draws an on-device confirmation and **awaits a tap**, so the command suspends back to
//!     JS (which paints the framebuffer and feeds touch input) until the user approves or
//!     rejects.
//!
//! Hand-rolled ABI (no wasm-bindgen):
//!   * `bitcoin_init()` builds the client+driver once.
//!   * `bitcoin_start_*` begin a command (none in flight).
//!   * `bitcoin_poll()` advances it: -1 while waiting for input, else the result length
//!     (a UTF-8 string written into `IO`: `fingerprint:<hex>`, `xpub:<base58>` or `error:<msg>`).
//!   * `bitcoin_push_touch` / `bitcoin_push_quit` queue input events.
//!   * `fb_*` expose the framebuffer for the page to paint to a <canvas>.

extern crate alloc;

use core::cell::Cell;
use core::ptr::addr_of_mut;
use core::task::Poll;
use std::cell::RefCell;

use sdk::executor::block_on;
use sdk::wasm_runtime;
use sdk::{App, AppBuilder};

use client::message::KeyTree;
use client::{BitcoinClient, WasmAppTransport, WasmClientDriver};

const IO_CAP: usize = 16 * 1024;
static mut IO: [u8; IO_CAP] = [0; IO_CAP];

thread_local! {
    static DRIVER: RefCell<Option<WasmClientDriver<BitcoinClient>>> = const { RefCell::new(None) };
    // Stable pointer to the co-resident app, for pumping its idle/dashboard UX between
    // commands. The app is owned (boxed) inside the transport inside the client inside DRIVER.
    static APP: Cell<*mut App> = const { Cell::new(core::ptr::null_mut()) };
}

#[no_mangle]
pub extern "C" fn bitcoin_init() {
    // The co-resident Bitcoin V-App, wrapped as a client transport, wrapped in the real
    // BitcoinClient, driven cooperatively.
    let mut transport = WasmAppTransport::new(AppBuilder::new(
        "Bitcoin",
        env!("CARGO_PKG_VERSION"),
        vnd_bitcoin::process_message,
    ));
    // Grab the app pointer before the transport moves into the client (the app is boxed, so
    // the pointer stays valid); used to pump the dashboard between commands.
    APP.with(|a| a.set(transport.app_ptr()));
    let btc_client = BitcoinClient::new(Box::new(transport));
    DRIVER.with(|d| *d.borrow_mut() = Some(WasmClientDriver::new(btc_client)));
}

/// Whether a client command is currently being driven (so the page pauses the idle pump).
fn command_in_flight() -> bool {
    DRIVER.with(|d| d.borrow().as_ref().map_or(true, |dr| dr.busy()))
}

#[no_mangle]
pub extern "C" fn bitcoin_busy() -> u32 {
    command_in_flight() as u32
}

// Drive one idle/dashboard UX step on the co-resident app. One queued event must be present
// (the callers below queue one first), so the step completes without awaiting.
fn drive_idle_one() {
    // SAFETY: single-threaded wasm, and we only run while no command is in flight (callers
    // check `command_in_flight`), so nothing else borrows the app. The app outlives this
    // pointer (owned by DRIVER for the life of the module).
    let p = APP.with(|a| a.get());
    if !p.is_null() {
        block_on(unsafe { &mut *p }.idle_ux_step());
    }
}

/// Pump one tick of the device clock while idle: draws the dashboard and advances its timers
/// (e.g. the return-to-dashboard countdown after a timed `show_info`). No-op while a command
/// runs. The page calls this on a ~100 ms timer.
#[no_mangle]
pub extern "C" fn bitcoin_tick() {
    if command_in_flight() {
        return;
    }
    wasm_runtime::push_ticker();
    drive_idle_one();
}

/// Feed a dashboard tap (press + release) while idle — drives the dashboard's app-info / quit
/// navigation. No-op while a command runs (command screens take taps via `bitcoin_push_touch`).
#[no_mangle]
pub extern "C" fn bitcoin_idle_touch(x: u32, y: u32) {
    if command_in_flight() {
        return;
    }
    wasm_runtime::push_touch(x as u16, y as u16, true);
    drive_idle_one(); // consume the press
    wasm_runtime::push_touch(x as u16, y as u16, false);
    drive_idle_one(); // consume the release (dashboard nav acts on release)
}

/// Begin `GetMasterFingerprint` (no UI; completes in one poll).
#[no_mangle]
pub extern "C" fn bitcoin_start_get_fingerprint() {
    DRIVER.with(|d| {
        d.borrow_mut()
            .as_mut()
            .expect("bitcoin_init must be called first")
            .start(|client| {
                Box::pin(async move {
                    match client.get_master_fingerprint(KeyTree::Standard).await {
                        Ok(fp) => alloc::format!("fingerprint:{fp:08x}").into_bytes(),
                        Err(e) => alloc::format!("error:{e}").into_bytes(),
                    }
                })
            })
    });
}

/// Begin `GetExtendedPubkey` at `m/84'/1'/0'`. With `display != 0` the app shows an on-device
/// confirmation and awaits a tap, so the command drives the interactive step loop.
#[no_mangle]
pub extern "C" fn bitcoin_start_get_pubkey(display: u32) {
    let display = display != 0;
    DRIVER.with(|d| {
        d.borrow_mut()
            .as_mut()
            .expect("bitcoin_init must be called first")
            .start(move |client| {
                Box::pin(async move {
                    match client
                        .get_extended_pubkey(KeyTree::Standard, "m/84'/1'/0'", display, None)
                        .await
                    {
                        Ok((xpub, _sig)) => {
                            let s = bitcoin::base58::encode_check(&xpub);
                            alloc::format!("xpub:{s}").into_bytes()
                        }
                        Err(e) => alloc::format!("error:{e}").into_bytes(),
                    }
                })
            })
    });
}

/// Advance the in-flight command: -1 while it waits for on-device input, else the result
/// length (a UTF-8 string written into `IO`).
#[no_mangle]
pub extern "C" fn bitcoin_poll() -> i64 {
    DRIVER.with(|d| {
        match d
            .borrow_mut()
            .as_mut()
            .expect("bitcoin_init must be called first")
            .poll()
        {
            Poll::Ready(out) => {
                let n = out.len().min(IO_CAP);
                let io = addr_of_mut!(IO) as *mut u8;
                // SAFETY: single-threaded wasm; JS only reads IO between our calls.
                unsafe { std::slice::from_raw_parts_mut(io, n) }.copy_from_slice(&out[..n]);
                n as i64
            }
            Poll::Pending => -1,
        }
    })
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
pub extern "C" fn bitcoin_push_touch(x: u32, y: u32, pressed: u32) {
    wasm_runtime::push_touch(x as u16, y as u16, pressed != 0);
}
#[no_mangle]
pub extern "C" fn bitcoin_push_quit() {
    wasm_runtime::push_quit();
}

// --- framebuffer accessors for the page (paints it to a <canvas>) ---
#[no_mangle]
pub extern "C" fn fb_ptr() -> *const u8 {
    wasm_runtime::framebuffer_ptr()
}
#[no_mangle]
pub extern "C" fn fb_width() -> usize {
    wasm_runtime::framebuffer_width()
}
#[no_mangle]
pub extern "C" fn fb_height() -> usize {
    wasm_runtime::framebuffer_height()
}
#[no_mangle]
pub extern "C" fn fb_version() -> u64 {
    wasm_runtime::framebuffer_version()
}
