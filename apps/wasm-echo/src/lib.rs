//! Minimal "echo" V-App compiled to WebAssembly, to validate architecture A: a real
//! V-App message handler running inside a wasm module and driven from JS.
//!
//! No wasm-bindgen — a tiny hand-rolled ABI so the module is easy to drive from a plain
//! `WebAssembly.instantiate` (see index.html / test.mjs):
//!   * a shared `IO` buffer in wasm memory for passing bytes both ways,
//!   * `app_init()` builds the app once,
//!   * `app_send(len)` runs the handler on `len` bytes from `IO` and returns the response
//!     length (written back into `IO`).
//!
//! The four `host_*` imports the SDK declares (print/random/exit/fatal) must be supplied
//! by the page in the `env` import namespace.

extern crate alloc;

use core::ptr::addr_of_mut;
use std::cell::RefCell;

use sdk::{App, AppBuilder};

const IO_CAP: usize = 16 * 1024;
static mut IO: [u8; IO_CAP] = [0; IO_CAP];

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// The V-App's message handler — the same seam `AppBuilder` drives on every target.
#[sdk::handler]
async fn process(_app: &mut App, msg: &[u8]) -> Vec<u8> {
    let mut out = b"echo:".to_vec();
    out.extend_from_slice(msg);
    out
}

/// Pointer to the shared IO buffer (JS writes the command here, reads the response here).
#[no_mangle]
pub extern "C" fn io_ptr() -> *mut u8 {
    addr_of_mut!(IO) as *mut u8
}

/// Capacity of the shared IO buffer.
#[no_mangle]
pub extern "C" fn io_cap() -> usize {
    IO_CAP
}

/// Builds the app once. Must be called before `app_send`.
#[no_mangle]
pub extern "C" fn app_init() {
    APP.with(|a| {
        *a.borrow_mut() = Some(AppBuilder::new("echo", env!("CARGO_PKG_VERSION"), process).build_wasm());
    });
}

/// Runs the handler on the first `len` bytes of `IO`, writes the response back into `IO`,
/// and returns its length.
#[no_mangle]
pub extern "C" fn app_send(len: usize) -> usize {
    let io = addr_of_mut!(IO) as *mut u8;
    // SAFETY: single-threaded wasm; JS only touches IO between our calls.
    let cmd = unsafe { std::slice::from_raw_parts(io, len.min(IO_CAP)) }.to_vec();
    let resp = APP.with(|a| {
        a.borrow_mut()
            .as_mut()
            .expect("app_init must be called before app_send")
            .dispatch_blocking(&cmd)
    });
    let n = resp.len().min(IO_CAP);
    unsafe { std::slice::from_raw_parts_mut(io, n) }.copy_from_slice(&resp[..n]);
    n
}
