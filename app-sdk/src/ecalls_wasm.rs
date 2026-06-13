//! WebAssembly backend for the ECALL surface (`target_wasm`).
//!
//! Implements the same ECALLs as the native backend, but for running a V-App inside a web
//! page — architecture A: the app and its client are co-resident in one wasm module,
//! driven cooperatively from JS (see the cooperative runtime). `wasm32-unknown-unknown`
//! has `std` (minus net/threads/fs/process), so this mirrors the native backend's shape
//! with I/O bridged to the JS host or kept in wasm memory.
//!
//! Status: skeleton. Message I/O, `print`, RNG, `exit`/`fatal` and device properties are
//! wired; crypto, display and storage ECALLs are stubs, to be filled in incrementally
//! (the crypto ones will be shared with the pure-Rust native implementations).

use std::sync::Mutex;

use common::ux::{EventCode, EventData};

// ---------------------------------------------------------------------------
// JS host imports — provided by the page when instantiating the module.
// ---------------------------------------------------------------------------
unsafe extern "C" {
    /// Writes `len` bytes of UTF-8 at `ptr` to the host console.
    fn host_print(ptr: *const u8, len: usize);
    /// Fills `len` bytes at `ptr` with cryptographically secure random data
    /// (e.g. `crypto.getRandomValues`).
    fn host_random(ptr: *mut u8, len: usize);
    /// Terminates the V-App with `status`. Never returns.
    fn host_exit(status: i32) -> !;
    /// Reports a fatal error (`len` bytes of UTF-8 at `ptr`) and terminates.
    fn host_fatal(ptr: *const u8, len: usize) -> !;
}

// ---------------------------------------------------------------------------
// Message channel between the V-App and the co-resident client/runtime.
// In the browser there is no socket: `xrecv`/`xsend` move bytes through these
// in-memory buffers, which the runtime fills/drains around each step.
// ---------------------------------------------------------------------------
static INBOX: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static OUTBOX: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Runtime hook: queue a message to be returned by the next `xrecv`.
pub(crate) fn push_inbox(msg: Vec<u8>) {
    *INBOX.lock().expect("INBOX poisoned") = msg;
}

/// Runtime hook: take whatever the last `xsend` produced.
pub(crate) fn take_outbox() -> Vec<u8> {
    std::mem::take(&mut *OUTBOX.lock().expect("OUTBOX poisoned"))
}

// ---------------------------------------------------------------------------
// Core I/O
// ---------------------------------------------------------------------------
pub fn exit(status: i32) -> ! {
    unsafe { host_exit(status) }
}

pub unsafe fn fatal(msg: *const u8, size: usize) -> ! {
    unsafe { host_fatal(msg, size) }
}

pub unsafe fn xsend(buffer: *const u8, size: usize) {
    // SAFETY: caller guarantees [buffer, buffer+size) is valid and readable.
    let data = unsafe { std::slice::from_raw_parts(buffer, size) };
    OUTBOX.lock().expect("OUTBOX poisoned").extend_from_slice(data);
}

pub unsafe fn xrecv(buffer: *mut u8, max_size: usize) -> usize {
    let mut inbox = INBOX.lock().expect("INBOX poisoned");
    let n = inbox.len().min(max_size);
    if n > 0 {
        // SAFETY: caller guarantees [buffer, buffer+max_size) is valid and writable.
        let dst = unsafe { std::slice::from_raw_parts_mut(buffer, n) };
        dst.copy_from_slice(&inbox[..n]);
        inbox.clear();
    }
    n
}

pub unsafe fn print(buffer: *const u8, size: usize) {
    unsafe { host_print(buffer, size) };
}

pub unsafe fn get_event(data: *mut EventData) -> u32 {
    // Skeleton: no input plumbing yet — always a ticker.
    unsafe { std::ptr::write(data, EventData::default()) };
    EventCode::Ticker as u32
}

pub fn get_device_property(property: u32) -> u32 {
    use common::ecall_constants::*;
    // A flex-like profile for now (480×600 Gray4, touch). The display backend will make
    // this configurable once it is wired to a canvas.
    match property {
        DEVICE_PROPERTY_ID => 0xFFFF_0002,
        DEVICE_PROPERTY_SCREEN_SIZE => (480u32 << 16) | 600,
        DEVICE_PROPERTY_FEATURES => FEATURE_TOUCH,
        DEVICE_PROPERTY_PIXEL_FORMAT => PixelFormat::Gray4 as u32,
        DEVICE_PROPERTY_DISPLAY_GRANULARITY => {
            DisplayGranularity { x: 1, y: 4, w: 1, h: 4 }.pack()
        }
        DEVICE_PROPERTY_MAX_TEXT_LEN => DISPLAY_MAX_TEXT_LEN as u32,
        DEVICE_PROPERTY_ABI_REVISION => VANADIUM_ABI_REVISION,
        _ => 0,
    }
}

pub unsafe fn get_random_bytes(buffer: *mut u8, size: usize) -> u32 {
    unsafe { host_random(buffer, size) };
    1
}

// ---------------------------------------------------------------------------
// Display — stubs, to be wired to a <canvas> via the runtime (reusing the webui
// frame/input protocol).
// ---------------------------------------------------------------------------
pub unsafe fn display_blit(
    _dst: u32,
    _size: u32,
    _buffer: *const u8,
    _buffer_len: usize,
    _src: u32,
    _src_stride: u32,
    _format: u32,
) -> i32 {
    0
}

pub unsafe fn display_refresh(_pos: u32, _size: u32, _mode: u32) -> i32 {
    0
}

pub unsafe fn display_fill_rect(_pos: u32, _size: u32, _color: u32) -> i32 {
    0
}

pub unsafe fn display_draw_text(
    _pos: u32,
    _size: u32,
    _text: *const u8,
    _text_len: usize,
    _font: u32,
    _color: u32,
    _bg: u32,
) -> i32 {
    0
}

pub unsafe fn display_text_width(_font: u32, _text: *const u8, _text_len: usize) -> i32 {
    0
}

pub unsafe fn display_font_metrics(_font: u32) -> i32 {
    0
}

// ---------------------------------------------------------------------------
// Storage — stub (IndexedDB/localStorage later).
// ---------------------------------------------------------------------------
pub unsafe fn storage_read(_slot_index: u32, _buffer: *mut u8, _buffer_size: usize) -> u32 {
    0
}

pub unsafe fn storage_write(_slot_index: u32, _buffer: *const u8, _buffer_size: usize) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Legacy page/step UX — not used on this target (the browser uses the low-level
// display primitives).
// ---------------------------------------------------------------------------
pub unsafe fn show_page(_page_desc: *const u8, _page_desc_len: usize) -> u32 {
    0
}

pub unsafe fn show_step(_step_desc: *const u8, _step_desc_len: usize) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Crypto / bignum / hash — stubs for now. These are pure-Rust on native (k256,
// bip32, sha2, …) and will be shared with this backend in a later step.
// ---------------------------------------------------------------------------
pub fn get_master_fingerprint(_curve: u32) -> u32 {
    todo!("get_master_fingerprint on wasm")
}

pub unsafe fn bn_modm(_r: *mut u8, _n: *const u8, _len: usize, _m: *const u8, _len_m: usize) -> u32 {
    todo!("bn_modm on wasm")
}

pub unsafe fn bn_addm(_r: *mut u8, _a: *const u8, _b: *const u8, _m: *const u8, _len: usize) -> u32 {
    todo!("bn_addm on wasm")
}

pub unsafe fn bn_subm(_r: *mut u8, _a: *const u8, _b: *const u8, _m: *const u8, _len: usize) -> u32 {
    todo!("bn_subm on wasm")
}

pub unsafe fn bn_multm(_r: *mut u8, _a: *const u8, _b: *const u8, _m: *const u8, _len: usize) -> u32 {
    todo!("bn_multm on wasm")
}

pub unsafe fn bn_powm(
    _r: *mut u8,
    _a: *const u8,
    _e: *const u8,
    _len_e: usize,
    _m: *const u8,
    _len: usize,
) -> u32 {
    todo!("bn_powm on wasm")
}

pub unsafe fn bn_modinv_prime(_r: *mut u8, _a: *const u8, _p: *const u8, _len: usize) -> u32 {
    todo!("bn_modinv_prime on wasm")
}

pub unsafe fn derive_hd_node(
    _curve: u32,
    _path: *const u32,
    _path_len: usize,
    _privkey: *mut u8,
    _chain_code: *mut u8,
) -> u32 {
    todo!("derive_hd_node on wasm")
}

pub unsafe fn derive_slip21_node(_labels: *const u8, _labels_len: usize, _out: *mut u8) -> u32 {
    todo!("derive_slip21_node on wasm")
}

pub unsafe fn ecfp_add_point(_curve: u32, _r: *mut u8, _p: *const u8, _q: *const u8) -> u32 {
    todo!("ecfp_add_point on wasm")
}

pub unsafe fn ecfp_scalar_mult(
    _curve: u32,
    _r: *mut u8,
    _p: *const u8,
    _k: *const u8,
    _k_len: usize,
) -> u32 {
    todo!("ecfp_scalar_mult on wasm")
}

pub unsafe fn ecdsa_sign(
    _curve: u32,
    _mode: u32,
    _hash_id: u32,
    _privkey: *const u8,
    _msg_hash: *const u8,
    _signature: *mut u8,
) -> usize {
    todo!("ecdsa_sign on wasm")
}

pub unsafe fn ecdsa_verify(
    _curve: u32,
    _pubkey: *const u8,
    _msg_hash: *const u8,
    _signature: *const u8,
    _signature_len: usize,
) -> u32 {
    todo!("ecdsa_verify on wasm")
}

pub unsafe fn schnorr_sign(
    _curve: u32,
    _mode: u32,
    _hash_id: u32,
    _privkey: *const u8,
    _msg: *const u8,
    _msg_len: usize,
    _signature: *mut u8,
    _entropy: *const [u8; 32],
) -> usize {
    todo!("schnorr_sign on wasm")
}

pub unsafe fn schnorr_verify(
    _curve: u32,
    _mode: u32,
    _hash_id: u32,
    _pubkey: *const u8,
    _msg: *const u8,
    _msg_len: usize,
    _signature: *const u8,
    _signature_len: usize,
) -> u32 {
    todo!("schnorr_verify on wasm")
}

pub unsafe fn hash_init(_hash_id: u32, _ctx: *mut u8) {
    todo!("hash_init on wasm")
}

pub unsafe fn hash_update(_hash_id: u32, _ctx: *mut u8, _data: *const u8, _len: usize) -> u32 {
    todo!("hash_update on wasm")
}

pub unsafe fn hash_final(_hash_id: u32, _ctx: *mut u8, _digest: *mut u8) -> u32 {
    todo!("hash_final on wasm")
}
