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
// Input event queue. The page pushes touch/button/quit events here; `get_event`
// pops them. When empty it returns `NO_EVENT_CODE`, which the SDK's async
// `ux::get_event` treats as "suspend": the handler future yields back to the JS
// step-driver, which renders the frame and waits for the next input.
// ---------------------------------------------------------------------------
/// Reserved code returned by `get_event` when the queue is empty (not a real
/// `EventCode`, so `EventCode::from` is never reached for it).
pub(crate) const NO_EVENT_CODE: u32 = u32::MAX - 1;

static EVENT_QUEUE: Mutex<std::collections::VecDeque<(EventCode, EventData)>> =
    Mutex::new(std::collections::VecDeque::new());

/// Runtime hook: queue an input event for the next `get_event`.
pub(crate) fn push_event(code: EventCode, data: EventData) {
    EVENT_QUEUE
        .lock()
        .expect("EVENT_QUEUE poisoned")
        .push_back((code, data));
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
    if let Some((code, ed)) = EVENT_QUEUE.lock().expect("EVENT_QUEUE poisoned").pop_front() {
        unsafe { std::ptr::write(data, ed) };
        code as u32
    } else {
        unsafe { std::ptr::write(data, EventData::default()) };
        NO_EVENT_CODE
    }
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
// Display — a software framebuffer (flex profile: 480×600 Gray4), ported from the
// native backend. `display_refresh` bumps a version; the page reads the framebuffer (via
// the `wasm_runtime` accessors) and paints it to a <canvas> — the webui frame protocol,
// but over direct memory reads instead of HTTP. (The native backend's full device-profile
// matrix will be shared with this backend later; for now it is fixed to flex.)
// ---------------------------------------------------------------------------
const FB_WIDTH: usize = 480;
const FB_HEIGHT: usize = 600;

struct Framebuffer {
    pixels: Vec<u8>, // intensity 0..=15, one byte per pixel
    version: u64,
}

static FB: Mutex<Framebuffer> = Mutex::new(Framebuffer {
    pixels: Vec::new(),
    version: 0,
});

fn with_fb<R>(f: impl FnOnce(&mut Framebuffer) -> R) -> R {
    let mut fb = FB.lock().expect("FB poisoned");
    if fb.pixels.len() != FB_WIDTH * FB_HEIGHT {
        fb.pixels = vec![0u8; FB_WIDTH * FB_HEIGHT];
    }
    f(&mut fb)
}

// Accessors for the runtime / page. The framebuffer is allocated once and never resized,
// so the pointer is stable; single-threaded wasm makes the unsynchronized JS read safe.
pub(crate) fn framebuffer_ptr() -> *const u8 {
    with_fb(|fb| fb.pixels.as_ptr())
}
pub(crate) fn framebuffer_dims() -> (usize, usize) {
    (FB_WIDTH, FB_HEIGHT)
}
pub(crate) fn framebuffer_version() -> u64 {
    FB.lock().expect("FB poisoned").version
}

// Flex (Gray4) accelerated-color quantization: the normative 4-entry palette, expanded to
// 4bpp like the device's EXPAND_TO_4BPP.
fn accel_intensity(rgb: u32) -> u8 {
    let c = common::ecall_constants::rgb888_to_palette(rgb);
    (c << 2) | c
}

fn decode_pixel(
    buffer: &[u8],
    format: common::ecall_constants::PixelFormat,
    stride: usize,
    row: usize,
    col: usize,
) -> u8 {
    use common::ecall_constants::PixelFormat;
    match format {
        PixelFormat::Mono1 => {
            let byte = buffer[row * stride + col / 8];
            let bit = 7 - (col % 8);
            if (byte >> bit) & 1 == 1 { 15 } else { 0 }
        }
        PixelFormat::Gray4 => {
            let byte = buffer[row * stride + col / 2];
            if col % 2 == 0 { byte >> 4 } else { byte & 0x0f }
        }
    }
}

pub unsafe fn display_blit(
    dst: u32,
    size: u32,
    buffer: *const u8,
    buffer_len: usize,
    src: u32,
    src_stride: u32,
    format: u32,
) -> i32 {
    use common::ecall_constants::*;
    let Some(format) = PixelFormat::from_u32(format) else {
        return display_unknown_enum_err(format);
    };
    let (x, y) = display_unpack_pair(dst);
    let (w, h) = display_unpack_pair(size);
    let (src_x, src_y) = display_unpack_pair(src);
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    let (src_x, src_y, src_stride) = (src_x as usize, src_y as usize, src_stride as usize);
    if w == 0 || h == 0 {
        return 0;
    }
    if x + w > FB_WIDTH || y + h > FB_HEIGHT {
        return DISPLAY_ERR_OUT_OF_BOUNDS;
    }
    // flex granularity (1,4,1,4)
    if y % 4 != 0 || h % 4 != 0 {
        return DISPLAY_ERR_ALIGNMENT;
    }
    let bpp = format.bits_per_pixel();
    let row_end = ((src_x + w) * bpp).div_ceil(8);
    let required = (src_y + h - 1) as u64 * src_stride as u64 + row_end as u64;
    if required > buffer_len as u64 {
        return DISPLAY_ERR_BAD_LAYOUT;
    }
    // SAFETY: caller guarantees [buffer, buffer+buffer_len) is valid and readable.
    let data = unsafe { std::slice::from_raw_parts(buffer, buffer_len) };
    with_fb(|fb| {
        for row in 0..h {
            for col in 0..w {
                let intensity = decode_pixel(data, format, src_stride, src_y + row, src_x + col);
                fb.pixels[(y + row) * FB_WIDTH + (x + col)] = intensity;
            }
        }
    });
    0
}

pub unsafe fn display_refresh(_pos: u32, size: u32, mode: u32) -> i32 {
    use common::ecall_constants::*;
    if RefreshMode::from_u32(mode).is_none() {
        return display_unknown_enum_err(mode);
    }
    let (w, h) = display_unpack_pair(size);
    if w == 0 || h == 0 {
        return 0;
    }
    // The whole framebuffer is presented; just publish a new version for the page.
    with_fb(|fb| fb.version += 1);
    0
}

pub unsafe fn display_fill_rect(pos: u32, size: u32, color: u32) -> i32 {
    use common::ecall_constants::*;
    if !rgb888_is_valid(color) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (x, y) = display_unpack_pair(pos);
    let (w, h) = display_unpack_pair(size);
    if w == 0 || h == 0 {
        return 0;
    }
    if (x + w) as usize > FB_WIDTH || (y + h) as usize > FB_HEIGHT {
        return DISPLAY_ERR_OUT_OF_BOUNDS;
    }
    let intensity = accel_intensity(color);
    with_fb(|fb| {
        for row in y as usize..(y + h) as usize {
            for col in x as usize..(x + w) as usize {
                fb.pixels[row * FB_WIDTH + col] = intensity;
            }
        }
    });
    0
}

// embedded-graphics font mapping + DrawTarget over the framebuffer (same approach as the
// native backend's best-effort text rasterization).
fn wasm_mono_font(font: u32) -> &'static embedded_graphics::mono_font::MonoFont<'static> {
    use common::ecall_constants::Font;
    use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X15, FONT_9X15_BOLD};
    match Font::from_u32(font) {
        Some(Font::Bold) => &FONT_9X15_BOLD,
        Some(Font::Large) => &FONT_10X20,
        _ => &FONT_9X15,
    }
}

fn wasm_font_dims(font: u32) -> (u32, u32, u32) {
    let f = wasm_mono_font(font);
    let cw = f.character_size.width + f.character_spacing;
    let h = f.character_size.height;
    (cw, h, h + 2)
}

struct FbTarget<'a> {
    fb: &'a mut Framebuffer,
}

impl embedded_graphics::draw_target::DrawTarget for FbTarget<'_> {
    type Color = embedded_graphics::pixelcolor::Gray4;
    type Error = core::convert::Infallible;
    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        use embedded_graphics::prelude::*;
        for embedded_graphics::Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && (p.x as usize) < FB_WIDTH && (p.y as usize) < FB_HEIGHT {
                self.fb.pixels[p.y as usize * FB_WIDTH + p.x as usize] = c.luma();
            }
        }
        Ok(())
    }
}

impl embedded_graphics::geometry::OriginDimensions for FbTarget<'_> {
    fn size(&self) -> embedded_graphics::geometry::Size {
        embedded_graphics::geometry::Size::new(FB_WIDTH as u32, FB_HEIGHT as u32)
    }
}

pub unsafe fn display_draw_text(
    pos: u32,
    size: u32,
    text: *const u8,
    text_len: usize,
    font: u32,
    color: u32,
    bg: u32,
) -> i32 {
    use common::ecall_constants::*;
    if Font::from_u32(font).is_none() {
        return display_unknown_enum_err(font);
    }
    if !rgb888_is_valid(color) || !rgb888_is_valid(bg) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (x, y) = display_unpack_pair(pos);
    let (w, h) = display_unpack_pair(size);
    if w == 0 || h == 0 {
        return 0;
    }
    if (x + w) as usize > FB_WIDTH || (y + h) as usize > FB_HEIGHT {
        return DISPLAY_ERR_OUT_OF_BOUNDS;
    }
    if text_len > DISPLAY_MAX_TEXT_LEN {
        return DISPLAY_ERR_TOO_LONG;
    }
    // SAFETY: caller guarantees [text, text+text_len) is valid readable memory.
    let bytes = unsafe { std::slice::from_raw_parts(text, text_len) };
    let Ok(s) = core::str::from_utf8(bytes) else {
        return DISPLAY_ERR_INVALID_ARG;
    };
    if bytes.contains(&0) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (cw, fh, _) = wasm_font_dims(font);
    let bg_i = accel_intensity(bg);
    let fg = accel_intensity(color);
    with_fb(|fb| {
        // Fill the box with the background.
        for row in y as usize..(y + h) as usize {
            for col in x as usize..(x + w) as usize {
                fb.pixels[row * FB_WIDTH + col] = bg_i;
            }
        }
        if fh <= h as u32 {
            use embedded_graphics::{
                mono_font::MonoTextStyle,
                pixelcolor::Gray4,
                prelude::*,
                text::{Baseline, Text},
            };
            let fit = (w / cw) as usize;
            let fitted: String = s.chars().take(fit).collect();
            let style = MonoTextStyle::new(wasm_mono_font(font), Gray4::new(fg));
            let mut target = FbTarget { fb };
            let _ = Text::with_baseline(
                &fitted,
                Point::new(x as i32, y as i32),
                style,
                Baseline::Top,
            )
            .draw(&mut target);
        }
    });
    0
}

pub unsafe fn display_text_width(font: u32, text: *const u8, text_len: usize) -> i32 {
    use common::ecall_constants::*;
    if Font::from_u32(font).is_none() {
        return display_unknown_enum_err(font);
    }
    if text_len > DISPLAY_MAX_TEXT_LEN {
        return DISPLAY_ERR_TOO_LONG;
    }
    // SAFETY: caller guarantees [text, text+text_len) is valid readable memory.
    let bytes = unsafe { std::slice::from_raw_parts(text, text_len) };
    let Ok(s) = core::str::from_utf8(bytes) else {
        return DISPLAY_ERR_INVALID_ARG;
    };
    if bytes.contains(&0) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (cw, _, _) = wasm_font_dims(font);
    (s.chars().count() as u32 * cw) as i32
}

pub unsafe fn display_font_metrics(font: u32) -> i32 {
    use common::ecall_constants::*;
    if Font::from_u32(font).is_none() {
        return display_unknown_enum_err(font);
    }
    let (_, height, line_height) = wasm_font_dims(font);
    ((height << 16) | line_height) as i32
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
