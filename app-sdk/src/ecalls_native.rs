use lazy_static::lazy_static;
use rand::TryRngCore;
use std::{
    io::{self, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    thread::sleep,
    time::Duration,
};

#[cfg(not(feature = "test-mode"))]
use std::io::Read;

use hmac::{Hmac, Mac};
use sha2::Sha512;

use sha2::Digest as _;

use common::{
    constants::STORAGE_SLOT_SIZE,
    ecall_constants::{
        CurveKind, HashId, CTX_RIPEMD160_SIZE, CTX_SHA256_SIZE, CTX_SHA3_SIZE, CTX_SHA512_SIZE,
        MAX_BIGNUMBER_SIZE,
    },
    ux::{Deserializable, EventCode, EventData},
    BufferType,
};

use bip32::{ChildNumber, XPrv};
use hex_literal::hex;
use k256::{
    ecdsa::{self, signature::hazmat::PrehashVerifier},
    elliptic_curve::{
        sec1::{FromEncodedPoint, ToEncodedPoint},
        Group, PrimeField,
    },
    schnorr, EncodedPoint, ProjectivePoint, Scalar,
};

use num_bigint::BigUint;
use num_traits::Zero;

// default seed used in Speculos, corresponding to the mnemonic "glory promote mansion idle axis finger extra february uncover one trip resource lawn turtle enact monster seven myth punch hobby comfort wild raise skin"
const DEFAULT_SEED: [u8; 64] = hex!("b11997faff420a331bb4a4ffdc8bdc8ba7c01732a99a30d83dbbebd469666c84b47d09d3f5f472b3b9384ac634beba2a440ba36ec7661144132f35e206873564");

const SLIP21_MAGIC: &'static str = "Symmetric key seed";

const TICKER_MS: u64 = 100;

unsafe fn to_bigint(bytes: *const u8, len: usize) -> BigUint {
    let bytes = unsafe {std::slice::from_raw_parts(bytes, len)};
    BigUint::from_bytes_be(bytes)
}

unsafe fn copy_result(r: *mut u8, result_bytes: &[u8], len: usize) -> () {
    unsafe {
        if result_bytes.len() < len {
            std::ptr::write_bytes(r, 0, len - result_bytes.len());
        }
        std::ptr::copy_nonoverlapping(
            result_bytes.as_ptr(),
            r.add(len - result_bytes.len()),
            result_bytes.len(),
        );
    }
}

// This should be called in show_page if there is no action for the user after showing the page.
fn epilogue_noaction() {
    // no action, just print the closing line
    println!("\n+=========================================+");
}

fn prompt_for_action(actions: &[(char, String)]) -> char {
    assert!(
        !actions.is_empty(),
        "This method requires at least one action"
    );

    let mut seen = std::collections::HashSet::new();
    for (ch, _) in actions {
        if !seen.insert(ch) {
            panic!("Duplicate action: {}", ch);
        }
    }

    println!("-------------------------------------------\n");
    println!("Actions:");
    for (c, desc) in actions {
        println!(" - {} : {}", desc, c);
    }
    // this assumes that prompt_for_action is called in show_page as the last thing after
    // showing the page; therefore, we print the closing line here.
    println!("\n+=========================================+");

    loop {
        let mut input = String::new();
        print!("$ ");
        io::stdout().flush().expect("Failed to flush stdout");
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");
        let trimmed = input.trim();
        if trimmed.len() == 1 {
            let ch = trimmed.chars().next().unwrap();
            if actions.iter().any(|(c, _)| *c == ch) {
                return ch;
            }
        }
        // show error and print the list of valid actions (only the character)
        print!("Invalid action. Valid actions are: ");
        for (i, (c, _)) in actions.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{}", c);
        }
        println!();
    }
}

/// Wait for the client to connect
fn wait_for_client() -> TcpStream {
    let addr = std::env::var("VAPP_ADDRESS").unwrap_or_else(|_| "127.0.0.1:2323".into());

    loop {
        match TcpListener::bind(&addr) {
            Ok(listener) => {
                eprintln!("V-App listening on {addr}, waiting for client...");

                // block until client connects
                match listener.accept() {
                    Ok((stream, remote)) => {
                        eprintln!("Client {remote} connected");
                        let _ = stream.set_nodelay(true);
                        return stream;
                    }
                    Err(err) => {
                        eprintln!("Accept failed ({err}). Retrying...");
                        sleep(Duration::from_millis(250));
                    }
                }
            }
            Err(err) => {
                eprintln!("Can’t bind {addr} ({err}). Retrying...");
                sleep(Duration::from_millis(250));
            }
        }
    }
}

lazy_static! {
    static ref LAST_EVENT: Mutex<Option<(common::ux::EventCode, common::ux::EventData)>> =
        Mutex::new(None);
    static ref TCP_CONN: Mutex<TcpStream> = Mutex::new(wait_for_client());
}

fn get_last_event() -> Option<(common::ux::EventCode, common::ux::EventData)> {
    let mut last_event = LAST_EVENT.lock().expect("Mutex poisoned");
    last_event.take()
}

fn store_new_event(event_code: common::ux::EventCode, event_data: common::ux::EventData) {
    let mut last_event = LAST_EVENT.lock().expect("Mutex poisoned");
    // Store the new event if there is no stored event,
    // or if the currently stored event is a ticker.
    if last_event.is_none()
        || last_event
            .as_ref()
            .map_or(false, |e| e.0 == common::ux::EventCode::Ticker)
    {
        *last_event = Some((event_code, event_data));
    }
}

pub fn exit(status: i32) -> ! {
    std::process::exit(status);
}

pub fn fatal(msg: *const u8, size: usize) -> ! {
    // SAFETY: caller guarantees [buffer, buffer+size) is valid.
    let data = unsafe { std::slice::from_raw_parts(msg, size) };

    // We need to communicate the panic to the client before exiting.
    // 1 byte message type, 4-byte big-endian length, then raw payload.
    let mut stream = TCP_CONN.lock().expect("TCP mutex poisoned");
    stream
        .write_all(&[BufferType::Panic as u8])
        .and_then(|_| stream.write_all(&(size as u32).to_be_bytes()))
        .and_then(|_| stream.write_all(data))
        .and_then(|_| stream.flush())
        .expect("TCP write failed");

    // No reason not to panic also here, since we have to exit anyway
    panic!("{}", std::str::from_utf8(data).unwrap());
}

#[cfg(feature = "test-mode")]
pub fn xsend(_buffer: *const u8, _size: usize) {
    panic!("Cannot send or receive bytes in native unit tests");
}

#[cfg(not(feature = "test-mode"))]
pub fn xsend(buffer: *const u8, size: usize) {
    // SAFETY: caller guarantees [buffer, buffer+size) is valid.
    let data = unsafe { std::slice::from_raw_parts(buffer, size) };

    // 1 byte message type, 4-byte big-endian length, then raw payload.
    let mut stream = TCP_CONN.lock().expect("TCP mutex poisoned");
    stream
        .write_all(&[BufferType::VAppMessage as u8])
        .and_then(|_| stream.write_all(&(size as u32).to_be_bytes()))
        .and_then(|_| stream.write_all(data))
        .and_then(|_| stream.flush())
        .expect("TCP write failed");
}

#[cfg(feature = "test-mode")]
pub fn xrecv(_buffer: *mut u8, _max_size: usize) -> usize {
    panic!("Cannot send or receive bytes in native unit tests");
}

#[cfg(not(feature = "test-mode"))]
pub fn xrecv(buffer: *mut u8, max_size: usize) -> usize {
    let mut stream = TCP_CONN.lock().expect("TCP mutex poisoned");

    // Read the 4-byte length header first.
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("TCP read failed");
    let expected = u32::from_be_bytes(len_buf) as usize;

    if expected > max_size {
        panic!(
            "Peer wants to send {} bytes but caller only provided {}-byte buffer",
            expected, max_size
        );
    }

    // Read the payload.
    let slice = unsafe { std::slice::from_raw_parts_mut(buffer, expected) };
    stream.read_exact(slice).expect("TCP read failed");
    expected
}

#[cfg(not(feature = "test-mode"))]
pub fn print(buffer: *const u8, size: usize) {
    // SAFETY: caller guarantees [buffer, buffer+size) is valid.
    let data = unsafe { std::slice::from_raw_parts(buffer, size) };

    // 1 byte message type, 4-byte big-endian length, then raw payload.
    let mut stream = TCP_CONN.lock().expect("TCP mutex poisoned");
    stream
        .write_all(&[BufferType::Print as u8])
        .and_then(|_| stream.write_all(&(size as u32).to_be_bytes()))
        .and_then(|_| stream.write_all(data))
        .and_then(|_| stream.flush())
        .expect("TCP write failed");
}

// When running unit tests, we should not perform network I/O on prints,
// since tests will run without a client.
#[cfg(feature = "test-mode")]
pub fn print(buffer: *const u8, size: usize) {
    // SAFETY: caller guarantees [buffer, buffer+size) is valid.
    let data = unsafe { std::slice::from_raw_parts(buffer, size) };
    // In tests, avoid network I/O; just emit the message to stderr for visibility.
    println!("{}", String::from_utf8_lossy(data));
}

// Parses one line of synthetic input (see `read_synthetic_event`) into an event.
// Returns `None` for an empty/unrecognized line (the caller then yields a Ticker).
fn parse_synthetic_event(line: &str) -> Option<(EventCode, EventData)> {
    use common::ux::{Action, ButtonEvent, TouchEvent, TouchState};
    let mut it = line.split_whitespace();
    let coords = |it: &mut std::str::SplitWhitespace| -> Option<(u16, u16)> {
        let x = it.next()?.parse().ok()?;
        let y = it.next()?.parse().ok()?;
        Some((x, y))
    };
    let (code, data) = match it.next()? {
        // Touch screen: "t x y" press, "u x y" release (last touched point).
        "t" | "touch" => {
            let (x, y) = coords(&mut it)?;
            let mut ed = EventData::default();
            ed.touch = TouchEvent::new(x, y, TouchState::Pressed);
            (EventCode::Touch, ed)
        }
        "u" | "up" | "release" => {
            let (x, y) = coords(&mut it)?;
            let mut ed = EventData::default();
            ed.touch = TouchEvent::new(x, y, TouchState::Released);
            (EventCode::Touch, ed)
        }
        // Nano buttons.
        "left" | "l" => button(ButtonEvent::LeftPress),
        "right" | "r" => button(ButtonEvent::RightPress),
        "both" | "b" => button(ButtonEvent::BothPress),
        // Quit the current custom-GUI loop.
        "q" | "quit" => {
            let mut ed = EventData::default();
            ed.action = Action::Quit;
            (EventCode::Action, ed)
        }
        _ => return None,
    };
    return Some((code, data));

    fn button(b: common::ux::ButtonEvent) -> (EventCode, EventData) {
        let mut ed = EventData::default();
        ed.button = b;
        (EventCode::Button, ed)
    }
}

// Reads one synthetic event from stdin, for interactively driving custom GUIs (e.g. the
// kolibri demo) on the native target. EOF is reported as a Quit action so a loop can end.
fn read_synthetic_event() -> Option<(EventCode, EventData)> {
    use std::io::BufRead;
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if n == 0 {
        // EOF: synthesize a Quit so the caller's event loop can terminate.
        let mut ed = EventData::default();
        ed.action = common::ux::Action::Quit;
        return Some((EventCode::Action, ed));
    }
    parse_synthetic_event(line.trim())
}

pub fn get_event(data: *mut EventData) -> u32 {
    if data.is_null() {
        panic!("The EventData pointer must not be null");
    }

    unsafe {
        if let Some((event_code, event_data)) = get_last_event() {
            std::ptr::write(data, event_data);
            return event_code as u32;
        }
    }

    // Optional synthetic input for interactively testing custom GUIs on native. Enabled by
    // setting VAPP_NATIVE_INPUT; otherwise the only event is the periodic ticker. Page-based
    // UX stores its action before calling get_event, so this branch never steals their input.
    if std::env::var_os("VAPP_NATIVE_INPUT").is_some() {
        if let Some((event_code, event_data)) = read_synthetic_event() {
            unsafe {
                std::ptr::write(data, event_data);
            }
            return event_code as u32;
        }
    }

    // We wait for TICKER_MS milliseconds and return a Ticker event.
    std::thread::sleep(std::time::Duration::from_millis(TICKER_MS));
    return EventCode::Ticker as u32;
}

// ===========================================================================
// Low-level graphics (display_blit)
//
// On the native target the "screen" is a virtual framebuffer kept in memory.
// Every blit is dumped to a PPM file so it can be inspected without any system
// dependency; when the optional `gui` feature is enabled it is also mirrored to
// an embedded-graphics-simulator window.
// ===========================================================================

// Native virtual screen geometry; mirrors a Stax-sized Gray4 display.
const NATIVE_SCREEN_WIDTH: usize = 400;
const NATIVE_SCREEN_HEIGHT: usize = 672;

struct VirtualScreen {
    width: usize,
    height: usize,
    // One byte per pixel, intensity 0..=15 (Gray4). Simpler than packing; the
    // native target is not memory-constrained.
    pixels: Vec<u8>,
}

impl VirtualScreen {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; width * height],
        }
    }

    // Writes the screen to a PPM (P6) file as 8-bit grayscale.
    fn dump_ppm(&self, path: &str) -> io::Result<()> {
        let mut out = Vec::with_capacity(self.width * self.height * 3 + 32);
        out.extend_from_slice(format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes());
        for &intensity in &self.pixels {
            // scale 0..=15 to 0..=255
            let v = ((intensity as u16) * 255 / 15) as u8;
            out.extend_from_slice(&[v, v, v]);
        }
        std::fs::write(path, out)
    }
}

lazy_static! {
    static ref VIRTUAL_SCREEN: Mutex<VirtualScreen> =
        Mutex::new(VirtualScreen::new(NATIVE_SCREEN_WIDTH, NATIVE_SCREEN_HEIGHT));
}

// Decodes the intensity (0..=15) of pixel (col, row) within a blit buffer.
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
            if (byte >> bit) & 1 == 1 {
                15
            } else {
                0
            }
        }
        PixelFormat::Gray4 => {
            let byte = buffer[row * stride + col / 2];
            if col % 2 == 0 {
                byte >> 4
            } else {
                byte & 0x0f
            }
        }
    }
}

pub fn display_blit(
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

    // Empty blit is a no-op success.
    if w == 0 || h == 0 {
        return 0;
    }

    let mut screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");

    // Validation mirrors the VM handler exactly — same checks, same order, same
    // codes — so a contract violation fails here too, not only on a real device.
    if x + w > screen.width || y + h > screen.height {
        return DISPLAY_ERR_OUT_OF_BOUNDS;
    }
    // The virtual screen has no hardware granularity, but the device does (NBGL's
    // 4-row constraint): enforce it so the violation is caught on the dev machine.
    if y % 4 != 0 || h % 4 != 0 {
        return DISPLAY_ERR_ALIGNMENT;
    }
    // Every byte the source rectangle addresses must lie within `buffer_len`: each
    // row spans the bytes [floor(src_x*bpp/8), ceil((src_x+w)*bpp/8)) from the start
    // of its row. `src_stride` itself is unconstrained (rows may overlap; 0
    // replicates a single row), so only the farthest addressed byte matters.
    let bpp = format.bits_per_pixel();
    let row_end = ((src_x + w) * bpp).div_ceil(8);
    let required = (src_y + h - 1) as u64 * src_stride as u64 + row_end as u64;
    if required > buffer_len as u64 {
        return DISPLAY_ERR_BAD_LAYOUT;
    }

    // SAFETY: caller guarantees [buffer, buffer+buffer_len) is valid and readable.
    let data = unsafe { std::slice::from_raw_parts(buffer, buffer_len) };

    let screen_width = screen.width;
    for row in 0..h {
        for col in 0..w {
            let intensity = decode_pixel(data, format, src_stride, src_y + row, src_x + col);
            screen.pixels[(y + row) * screen_width + (x + col)] = intensity;
        }
    }

    // Note: only the virtual framebuffer is updated here. The viewable copy (PPM /
    // simulator window) is produced by `display_refresh`, mirroring the device,
    // where drawing is decoupled from pushing the panel.
    0
}

pub fn display_refresh(pos: u32, size: u32, mode: u32) -> i32 {
    use common::ecall_constants::*;

    // The refresh mode only affects the physical e-ink panel; on the virtual screen we
    // just validate it and dump the framebuffer regardless.
    if RefreshMode::from_u32(mode).is_none() {
        return display_unknown_enum_err(mode);
    }
    let (_x, _y) = display_unpack_pair(pos);
    let (w, h) = display_unpack_pair(size);
    if w == 0 || h == 0 {
        return 0;
    }
    // The rectangle is advisory (the VM self-aligns and clips it, never erroring), and
    // the virtual screen always presents the whole framebuffer, so there is nothing to
    // validate here.
    let screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");

    // Persist a viewable copy of the whole screen.
    let path = std::env::var("VAPP_SCREEN_PPM").unwrap_or_else(|_| "vapp_screen.ppm".into());
    let _ = screen.dump_ppm(&path);

    #[cfg(feature = "gui")]
    gui::present(&screen);

    0
}

// ---------------------------------------------------------------------------
// Accelerated draw ops. On device these forward to native NBGL drawing in the OS
// framebuffer; here we approximate them on the virtual screen so PPM/simulator output
// stays representative. Text and QR codes need OS fonts/encoders we don't replicate, so
// they are best-effort placeholders (the device/Speculos is the reference for those).
// ---------------------------------------------------------------------------

// Fills `[x, x+w) × [y, y+h)` (clipped to the screen) with `intensity` (0..=15).
fn fill_screen_rect(x: usize, y: usize, w: usize, h: usize, intensity: u8) {
    let mut screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");
    let (sw, sh) = (screen.width, screen.height);
    let x1 = (x + w).min(sw);
    let y1 = (y + h).min(sh);
    for row in y..y1 {
        for col in x..x1 {
            screen.pixels[row * sw + col] = intensity;
        }
    }
}

// The Gray4 intensity an accelerated-path RGB888 color renders as: the normative
// 4-entry palette quantization, expanded like the device's `EXPAND_TO_4BPP`
// (`(c << 2) | c`). Deliberately 4 levels, not 16 — the accelerated ops are
// palette-limited on the device, and the virtual screen mirrors that.
fn accel_rgb_intensity(rgb: u32) -> u8 {
    let c = common::ecall_constants::rgb888_to_palette(rgb);
    (c << 2) | c
}

pub fn display_fill_rect(pos: u32, size: u32, color: u32) -> i32 {
    use common::ecall_constants::*;

    // Colors are RGB888 and quantized, never rejected — only the reserved top byte
    // is checked.
    if !rgb888_is_valid(color) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (x, y) = display_unpack_pair(pos);
    let (w, h) = display_unpack_pair(size);
    if w == 0 || h == 0 {
        return 0;
    }
    {
        let screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");
        if (x + w) as usize > screen.width || (y + h) as usize > screen.height {
            return DISPLAY_ERR_OUT_OF_BOUNDS;
        }
    }
    fill_screen_rect(
        x as usize,
        y as usize,
        w as usize,
        h as usize,
        accel_rgb_intensity(color),
    );
    0
}

// Maps a device [`Font`](common::ecall_constants::Font) id to an embedded-graphics
// monospace font of comparable size, for the native backend's best-effort text rendering
// and measurement. The device uses real OS fonts; this only needs to look representative.
#[cfg(feature = "embedded-graphics")]
fn native_mono_font(font: u32) -> &'static embedded_graphics::mono_font::MonoFont<'static> {
    use common::ecall_constants::Font;
    use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X15, FONT_9X15_BOLD};
    match Font::from_u32(font) {
        Some(Font::Bold) => &FONT_9X15_BOLD,
        Some(Font::Large) => &FONT_10X20,
        _ => &FONT_9X15, // Regular / unknown
    }
}

// (per-character advance width, glyph height, line height) in pixels for a device font.
// Used so the native backend's `display_text_width` / `display_font_metrics` stay
// consistent with what `display_draw_text` renders.
fn native_font_dims(font: u32) -> (u32, u32, u32) {
    #[cfg(feature = "embedded-graphics")]
    {
        let f = native_mono_font(font);
        let cw = f.character_size.width + f.character_spacing;
        let h = f.character_size.height;
        (cw, h, h + 2)
    }
    #[cfg(not(feature = "embedded-graphics"))]
    {
        let _ = font;
        (9, 15, 17)
    }
}

// A `DrawTarget` over the native virtual screen, so embedded-graphics can rasterize text
// into it (the device rasterizes natively via NBGL; here we approximate for the PPM).
#[cfg(feature = "embedded-graphics")]
struct VsTarget<'a> {
    screen: &'a mut VirtualScreen,
}

#[cfg(feature = "embedded-graphics")]
impl embedded_graphics::draw_target::DrawTarget for VsTarget<'_> {
    type Color = embedded_graphics::pixelcolor::Gray4;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        use embedded_graphics::prelude::*;
        let (w, h) = (self.screen.width as i32, self.screen.height as i32);
        for embedded_graphics::Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && p.x < w && p.y < h {
                let idx = p.y as usize * self.screen.width + p.x as usize;
                self.screen.pixels[idx] = c.luma();
            }
        }
        Ok(())
    }
}

#[cfg(feature = "embedded-graphics")]
impl embedded_graphics::geometry::OriginDimensions for VsTarget<'_> {
    fn size(&self) -> embedded_graphics::geometry::Size {
        embedded_graphics::geometry::Size::new(self.screen.width as u32, self.screen.height as u32)
    }
}

pub fn display_draw_text(
    pos: u32,
    size: u32,
    text: *const u8,
    text_len: usize,
    font: u32,
    color: u32,
    bg: u32,
) -> i32 {
    use common::ecall_constants::*;

    // Validation mirrors the VM handler: same checks, same order, same codes.
    if Font::from_u32(font).is_none() {
        return display_unknown_enum_err(font);
    }
    // Colors are RGB888 and quantized, never rejected — only the reserved top byte
    // is checked.
    if !rgb888_is_valid(color) || !rgb888_is_valid(bg) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (x, y) = display_unpack_pair(pos);
    let (w, h) = display_unpack_pair(size);
    // An empty clip box draws nothing.
    if w == 0 || h == 0 {
        return 0;
    }
    {
        let screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");
        if (x + w) as usize > screen.width || (y + h) as usize > screen.height {
            return DISPLAY_ERR_OUT_OF_BOUNDS;
        }
    }
    if text_len > DISPLAY_MAX_TEXT_LEN {
        return DISPLAY_ERR_TOO_LONG;
    }
    // SAFETY: caller guarantees [text, text+text_len) is valid readable memory.
    let bytes = unsafe { std::slice::from_raw_parts(text, text_len) };
    let Ok(_s) = core::str::from_utf8(bytes) else {
        return DISPLAY_ERR_INVALID_ARG;
    };
    if bytes.contains(&0) {
        return DISPLAY_ERR_INVALID_ARG;
    }

    // Defined rendering semantics, mirroring the device: the box is filled with `bg`,
    // and glyphs are clipped to it — none drawn if the box is shorter than the font,
    // truncated at the last fitting glyph if the text is wider than the box (using the
    // same per-character advance `display_text_width` reports, so the two agree).
    fill_screen_rect(
        x as usize,
        y as usize,
        w as usize,
        h as usize,
        accel_rgb_intensity(bg),
    );
    let (_cw, fh, _) = native_font_dims(font);
    if fh > h {
        return 0;
    }
    #[cfg(feature = "embedded-graphics")]
    {
        use embedded_graphics::{
            mono_font::MonoTextStyle,
            pixelcolor::Gray4,
            prelude::*,
            text::{Baseline, Text},
        };
        let fit = (w / _cw) as usize;
        let fitted: std::string::String = _s.chars().take(fit).collect();
        let style = MonoTextStyle::new(native_mono_font(font), Gray4::new(accel_rgb_intensity(color)));
        let mut screen = VIRTUAL_SCREEN.lock().expect("Screen mutex poisoned");
        let mut target = VsTarget {
            screen: &mut screen,
        };
        // The device positions text within the (x, y, w, h) box from the top-left; mirror
        // that with a Top baseline at (x, y).
        let _ = Text::with_baseline(&fitted, Point::new(x as i32, y as i32), style, Baseline::Top)
            .draw(&mut target);
    }
    0
}

pub fn display_text_width(font: u32, text: *const u8, text_len: usize) -> i32 {
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
    // Interior NULs are rejected like on device (the backing nbgl_getTextWidth
    // syscall takes a NUL-terminated string).
    if bytes.contains(&0) {
        return DISPLAY_ERR_INVALID_ARG;
    }
    let (cw, _, _) = native_font_dims(font);
    (s.chars().count() as u32 * cw) as i32
}

pub fn display_font_metrics(font: u32) -> i32 {
    use common::ecall_constants::*;

    if Font::from_u32(font).is_none() {
        return display_unknown_enum_err(font);
    }
    let (_, height, line_height) = native_font_dims(font);
    ((height << 16) | line_height) as i32
}

#[cfg(feature = "gui")]
mod gui {
    use super::VirtualScreen;
    use embedded_graphics::pixelcolor::Gray8;
    use embedded_graphics::prelude::*;
    use embedded_graphics_simulator::{OutputSettingsBuilder, SimulatorDisplay, Window};
    use std::sync::Mutex;

    lazy_static::lazy_static! {
        static ref WINDOW: Mutex<Option<Window>> = Mutex::new(None);
    }

    pub fn present(screen: &VirtualScreen) {
        let mut display: SimulatorDisplay<Gray8> =
            SimulatorDisplay::new(Size::new(screen.width as u32, screen.height as u32));
        for (i, &intensity) in screen.pixels.iter().enumerate() {
            let x = (i % screen.width) as i32;
            let y = (i / screen.width) as i32;
            let v = ((intensity as u16) * 255 / 15) as u8;
            let _ = Pixel(Point::new(x, y), Gray8::new(v)).draw(&mut display);
        }
        let mut guard = WINDOW.lock().expect("Window mutex poisoned");
        let window = guard.get_or_insert_with(|| {
            Window::new("Vanadium V-App", &OutputSettingsBuilder::new().build())
        });
        window.update(&display);
    }
}

pub fn show_page(page_desc: *const u8, page_desc_len: usize) -> u32 {
    // make a slice from page_desc and page_desc_len
    let page_desc_slice = unsafe { std::slice::from_raw_parts(page_desc, page_desc_len) };

    let Ok(page_desc) = common::ux::Page::deserialize_full(page_desc_slice) else {
        return 0;
    };

    println!("\n+=========================================+");
    match page_desc {
        common::ux::Page::Spinner { text } => {
            println!("{}...", text);
            epilogue_noaction();
        }
        common::ux::Page::Info { icon, text } => {
            match icon {
                common::ux::Icon::None => println!("{}", text),
                common::ux::Icon::Success => println!("✓ {}", text),
                common::ux::Icon::Failure => println!("❌ {}", text),
                // The following should not happen on the native target, since they are used for
                // small devices that implement the step UX model.
                common::ux::Icon::Confirm => println!("{}", text),
                common::ux::Icon::Reject => println!("{}", text),
                common::ux::Icon::Processing => println!("{}", text),
            }
            epilogue_noaction();
        }
        common::ux::Page::ConfirmReject {
            title,
            text,
            confirm,
            reject,
        } => {
            println!("{}\n{}", title, text);

            let actions = vec![('C', confirm.to_string()), ('R', reject.to_string())];
            store_new_event(
                common::ux::EventCode::Action,
                common::ux::EventData {
                    action: match prompt_for_action(&actions) {
                        'C' => common::ux::Action::Confirm,
                        'R' => common::ux::Action::Reject,
                        _ => panic!("Unexpected action"),
                    },
                },
            );
        }
        common::ux::Page::GenericPage {
            navigation_info,
            page_content_info,
        } => {
            let mut actions: Vec<(char, String)> = vec![];

            if let Some(title_text) = page_content_info.title {
                actions.push(('B', "Back".into()));
                println!("{}", title_text);
            }

            match page_content_info.page_content {
                common::ux::PageContent::TextSubtext { text, subtext } => {
                    println!("{}\n{}", text, subtext);
                }
                common::ux::PageContent::TagValueList { list } => {
                    for tag_value in list {
                        println!("{}: {}", tag_value.tag, tag_value.value);
                    }
                }
                common::ux::PageContent::ConfirmationButton { text, button_text } => {
                    println!("{}", text);
                    actions.push(('C', button_text.into()));
                }
                common::ux::PageContent::ConfirmationLongPress {
                    text,
                    long_press_text,
                } => {
                    println!("{}", text);
                    actions.push(('C', long_press_text.into()));
                }
            }

            if let Some(navigation_info) = navigation_info {
                let mut can_go_back = false;
                match navigation_info.nav_info {
                    common::ux::NavInfo::NavWithButtons {
                        has_back_button,
                        has_page_indicator: _,
                        quit_text,
                    } => {
                        if !has_back_button {
                            can_go_back = false;
                        }
                        if let Some(quit_text) = quit_text {
                            actions.push(('Q', quit_text.into()));
                        }
                    }
                }

                println!(
                    "\nPage {} of {}\n",
                    navigation_info.active_page + 1,
                    navigation_info.n_pages
                );
                if can_go_back && navigation_info.active_page > 0 {
                    actions.push(('P', "Previous page".into()));
                }
                if navigation_info.active_page < navigation_info.n_pages - 1 {
                    actions.push(('N', "Next page".into()));
                }
            }

            if actions.len() > 0 {
                store_new_event(
                    common::ux::EventCode::Action,
                    common::ux::EventData {
                        action: match prompt_for_action(&actions) {
                            'P' => common::ux::Action::PreviousPage,
                            'N' => common::ux::Action::NextPage,
                            'C' => common::ux::Action::Confirm,
                            'Q' => common::ux::Action::Quit,
                            _ => panic!("Unexpected action"),
                        },
                    },
                );
            } else {
                // no actions, just print the closing line
                epilogue_noaction();
            }
        }
        common::ux::Page::Home { description } => {
            println!("{}", description);
            epilogue_noaction();
        }
    }

    1
}

pub fn show_step(_step_desc: *const u8, _step_desc_len: usize) -> u32 {
    panic!("The native target implements the page UX model, not the step UX model.");
}

pub fn get_device_property(property: u32) -> u32 {
    use common::ecall_constants::*;
    match property {
        // The native pseudo-device: vendor 0xFFFF, product 1 (nonzero per the
        // property contract — 0 always means "unsupported property").
        DEVICE_PROPERTY_ID => 0xFFFF_0001,
        DEVICE_PROPERTY_SCREEN_SIZE => {
            ((NATIVE_SCREEN_WIDTH as u32) << 16) | (NATIVE_SCREEN_HEIGHT as u32)
        }
        // The virtual screen mirrors a touch-screen (Stax-like) device.
        DEVICE_PROPERTY_FEATURES => {
            FEATURE_TOUCH
                | FEATURE_ACCEL_RECT
                | FEATURE_ACCEL_TEXT
                | FEATURE_PARTIAL_REFRESH
                | FEATURE_FAST_MONO_REFRESH
        }
        DEVICE_PROPERTY_PIXEL_FORMAT => PixelFormat::Gray4 as u32,
        // Same 4-row granularity as the NBGL devices (display_blit enforces it here
        // too, so violations are caught on the dev machine).
        DEVICE_PROPERTY_DISPLAY_GRANULARITY => DisplayGranularity {
            x: 1,
            y: 4,
            w: 1,
            h: 4,
        }
        .pack(),
        DEVICE_PROPERTY_MAX_TEXT_LEN => DISPLAY_MAX_TEXT_LEN as u32,
        DEVICE_PROPERTY_ABI_REVISION => VANADIUM_ABI_REVISION,
        // Unknown properties return 0 (the probing contract), never panic.
        _ => 0,
    }
}

pub fn bn_modm(r: *mut u8, n: *const u8, len: usize, m: *const u8, len_m: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE || len_m > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    if len_m > len_m {
        return 0;
    }

    let n = unsafe { to_bigint(n, len) };
    let m = unsafe { to_bigint(m, len_m) };

    if m.is_zero() {
        return 0;
    }

    let result = n % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_addm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = (a + b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_subm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    // the `+ &m` is to avoid negative numbers, since BigUints must be non-negative
    let result = ((a + &m) - b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_multm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = (a * b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

/// Computes the modular inverse of `a` modulo `p`, storing the result in `r`.
/// The modulus `p` must be a prime number.
/// Uses Fermat's little theorem: a^{-1} = a^{p-2} mod p.
pub fn bn_modinv_prime(r: *mut u8, a: *const u8, p: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let p = unsafe { to_bigint(p, len) };

    if a.is_zero() || p.is_zero() {
        return 0;
    }

    if a >= p {
        return 0;
    }

    // Fermat's little theorem: a^{-1} = a^{p-2} mod p (valid when p is prime)
    let exp = &p - BigUint::from(2u32);
    let result = a.modpow(&exp, &p);
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_powm(
    r: *mut u8,
    a: *const u8,
    e: *const u8,
    len_e: usize,
    m: *const u8,
    len: usize,
) -> u32 {
    if len > MAX_BIGNUMBER_SIZE || len_e > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let e = unsafe { to_bigint(e, len_e) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = a.modpow(&e, &m);
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn derive_hd_node(
    curve: u32,
    path: *const u32,
    path_len: usize,
    privkey: *mut u8,
    chain_code: *mut u8,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }
    let mut key = get_master_bip32_key();

    let path_slice = unsafe { std::slice::from_raw_parts(path, path_len) };
    for path_step in path_slice {
        let child = ChildNumber::from(*path_step);
        key = match key.derive_child(child) {
            Ok(k) => k,
            Err(_) => return 0,
        };
    }

    // Copy the private key and chain code to the output buffers
    let privkey_bytes = key.private_key().to_bytes();
    let chain_code_bytes = key.attrs().chain_code;

    unsafe {
        std::ptr::copy_nonoverlapping(privkey_bytes.as_ptr(), privkey, privkey_bytes.len());
        std::ptr::copy_nonoverlapping(
            chain_code_bytes.as_ptr(),
            chain_code,
            chain_code_bytes.len(),
        );
    }

    1
}

pub fn get_master_fingerprint(curve: u32) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    u32::from_be_bytes(get_master_bip32_key().public_key().fingerprint())
}

pub fn derive_slip21_node(labels: *const u8, labels_len: usize, out: *mut u8) -> u32 {
    if out.is_null() {
        return 0;
    }

    // Vanadium uses a custom seed for its SLIP-21 hierarchy, for compatibility with Bolos
    // The seed is derived from a master secret using the standard SLIP-21 derivation.
    let custom_slip21_seed = slip21_custom_get_seed();

    let mut current_node = slip21_get_master_node(&custom_slip21_seed);

    if labels_len > 256 {
        return 0;
    }

    let labels: &[u8] = unsafe { std::slice::from_raw_parts(labels, labels_len) };

    // parse the `labels` buffer as the concatenation of a list of labels, each prefixed by its length
    // The length of each label is between 0 and 252 bytes, and the total length of the labels buffer must be
    // at most 256 bytes.

    let mut offset = 0;
    while offset < labels_len {
        if offset >= labels_len {
            return 0; // Buffer underrun
        }

        let label_len = labels[offset] as usize;
        offset += 1;

        if label_len > 252 {
            return 0; // Label too long
        }

        if offset + label_len > labels_len {
            return 0; // Buffer overrun
        }

        let label = &labels[offset..offset + label_len];
        offset += label_len;

        current_node = slip21_derive_child_node(&current_node, label);
    }

    unsafe {
        std::ptr::copy_nonoverlapping(current_node.as_ptr(), out, current_node.len());
    }

    1
}

pub fn ecfp_add_point(curve: u32, r: *mut u8, p: *const u8, q: *const u8) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        return 0;
    }

    let p_slice = unsafe { std::slice::from_raw_parts(p, 65) };
    let q_slice = unsafe { std::slice::from_raw_parts(q, 65) };

    // Helper to validate a point and copy it to the result
    let validate_and_copy = |point_slice: &[u8], source: *const u8| -> u32 {
        let encoded = match EncodedPoint::from_bytes(point_slice) {
            Ok(enc) => enc,
            Err(_) => return 0,
        };
        if ProjectivePoint::from_encoded_point(&encoded)
            .is_none()
            .into()
        {
            return 0;
        }
        // Use ptr::copy (memmove semantics) to handle the case where source == r.
        unsafe {
            std::ptr::copy(source, r, 65);
        }
        1
    };

    // Handle point at infinity: represented as prefix byte 0x00
    match (p_slice[0] == 0x00, q_slice[0] == 0x00) {
        (true, true) => {
            unsafe {
                std::ptr::write_bytes(r, 0, 65);
            }
            return 1;
        }
        (true, false) => return validate_and_copy(q_slice, q),
        (false, true) => return validate_and_copy(p_slice, p),
        (false, false) => { /* Continue with normal addition */ }
    }

    // Validate and decode point P
    let p_point = match EncodedPoint::from_bytes(p_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let p_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&p_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    // Validate and decode point Q
    let q_point = match EncodedPoint::from_bytes(q_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let q_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&q_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    let result_point: ProjectivePoint = p_point + q_point;

    // Check if result is the point at infinity
    // The k256 library may panic when encoding the point at infinity,
    // so we check for it first
    if bool::from(result_point.is_identity()) {
        // Encode point at infinity as all zeros (65 bytes)
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    let result_encoded = result_point.to_encoded_point(false);
    let result_bytes = result_encoded.as_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, result_bytes.len());
    }

    1
}

pub fn ecfp_scalar_mult(curve: u32, r: *mut u8, p: *const u8, k: *const u8, k_len: usize) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        return 0;
    }
    if k_len > 32 {
        return 0;
    }

    let p_slice = unsafe { std::slice::from_raw_parts(p, 65) };
    let k_slice = unsafe { std::slice::from_raw_parts(k, k_len) };

    // Handle point at infinity: represented as prefix byte 0x00
    if p_slice[0] == 0x00 {
        // O * k = O
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    // Validate and decode point P
    let p_point = match EncodedPoint::from_bytes(p_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let p_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&p_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    // pad k_scalar to 32 bytes with initial zeros without using unsafe code
    let mut k_scalar = [0u8; 32];
    k_scalar[32 - k_len..].copy_from_slice(k_slice);
    let k_scalar: Scalar = match Scalar::from_repr(k_scalar.into()).into() {
        Some(scalar) => scalar,
        None => return 0,
    };

    let result_point: ProjectivePoint = p_point * k_scalar;

    // Check if result is the point at infinity
    if bool::from(result_point.is_identity()) {
        // Encode point at infinity as all zeros (65 bytes)
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    let result_encoded = result_point.to_encoded_point(false);
    let result_bytes = result_encoded.as_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, result_bytes.len());
    }

    1
}

pub fn get_random_bytes(buffer: *mut u8, size: usize) -> u32 {
    if size == 0 {
        return 1;
    }
    if size > 256 {
        panic!("size is too large");
    }

    let mut rng = rand::rngs::OsRng::default();
    let mut random_bytes = [0u8; 256];
    rng.try_fill_bytes(&mut random_bytes[..size])
        .expect("Failed to generate random bytes");

    unsafe {
        std::ptr::copy_nonoverlapping(random_bytes.as_ptr(), buffer, size);
    }

    1
}

pub fn ecdsa_sign(
    curve: u32,
    mode: u32,
    hash_id: u32,
    privkey: *const u8,
    msg_hash: *const u8,
    signature: *mut u8,
) -> usize {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::EcdsaSignMode::RFC6979 as u32 {
        panic!("Invalid or unsupported ecdsa signing mode");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    let privkey_slice = unsafe { std::slice::from_raw_parts(privkey, 32) };
    let msg_hash_slice = unsafe { std::slice::from_raw_parts(msg_hash, 32) };

    let mut privkey_bytes = [0u8; 32];
    privkey_bytes[..].copy_from_slice(privkey_slice);
    let signing_key =
        ecdsa::SigningKey::from_bytes(&privkey_bytes.into()).expect("Invalid private key");
    let (signature_local, _) = signing_key
        .sign_prehash_recoverable(msg_hash_slice)
        .expect("Signing failed");

    let signature_der = ecdsa::DerSignature::from(signature_local);

    let signature_bytes = signature_der.to_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(signature_bytes.as_ptr(), signature, signature_bytes.len());
    }

    signature_bytes.len()
}

pub fn ecdsa_verify(
    curve: u32,
    pubkey: *const u8,
    msg_hash: *const u8,
    signature: *const u8,
    signature_len: usize,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if signature_len > 72 {
        panic!("signature_len is too large");
    }

    let pubkey_slice = unsafe { std::slice::from_raw_parts(pubkey, 65) };
    let msg_hash_slice = unsafe { std::slice::from_raw_parts(msg_hash, 32) };
    let signature_slice = unsafe { std::slice::from_raw_parts(signature, signature_len) };

    let pubkey_point = EncodedPoint::from_bytes(pubkey_slice).expect("Invalid public key");
    let verifying_key = ecdsa::VerifyingKey::from_encoded_point(&pubkey_point)
        .expect("Failed to create verifying key");

    let signature =
        ecdsa::DerSignature::from_bytes(signature_slice.into()).expect("Invalid signature");

    match verifying_key.verify_prehash(msg_hash_slice, &signature) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

pub fn schnorr_sign(
    curve: u32,
    mode: u32,
    hash_id: u32,
    privkey: *const u8,
    msg: *const u8,
    msg_len: usize,
    signature: *mut u8,
    entropy: *const [u8; 32],
) -> usize {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::SchnorrSignMode::BIP340 as u32 {
        panic!("Invalid or unsupported schnorr signing mode");
    }

    if msg_len > 128 {
        panic!("msg_len is too large");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    let privkey_slice = unsafe { std::slice::from_raw_parts(privkey, 32) };
    let msg_slice = unsafe { std::slice::from_raw_parts(msg, msg_len) };

    let mut privkey_bytes = [0u8; 32];
    privkey_bytes[..].copy_from_slice(privkey_slice);
    let signing_key = schnorr::SigningKey::from_bytes(&privkey_bytes).expect("Invalid private key");

    let aux_rand = if entropy.is_null() {
        // generate 32 random bytes
        let mut aux_rand = [0u8; 32];
        rand::rngs::OsRng::default()
            .try_fill_bytes(&mut aux_rand)
            .expect("Failed to generate random bytes");
        aux_rand
    } else {
        unsafe { *entropy }
    };

    let signature_bytes = signing_key
        .sign_raw(msg_slice, &aux_rand)
        .unwrap()
        .to_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(signature_bytes.as_ptr(), signature, signature_bytes.len());
    }

    signature_bytes.len()
}

pub fn schnorr_verify(
    curve: u32,
    mode: u32,
    hash_id: u32,
    pubkey: *const u8,
    msg: *const u8,
    msg_len: usize,
    signature: *const u8,
    signature_len: usize,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::SchnorrSignMode::BIP340 as u32 {
        panic!("Invalid or unsupported schnorr signing mode");
    }

    if msg_len > 128 {
        panic!("msg_len is too large");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    if signature_len != 64 {
        panic!("Invalid signature length");
    }

    let pubkey_slice = unsafe { std::slice::from_raw_parts(pubkey, 65) };
    let xonly_pubkey_slice = &pubkey_slice[1..33];
    let msg_slice = unsafe { std::slice::from_raw_parts(msg, msg_len) };
    let signature_slice = unsafe { std::slice::from_raw_parts(signature, signature_len) };

    let verifying_key =
        schnorr::VerifyingKey::from_bytes(xonly_pubkey_slice).expect("Invalid public key");
    let signature = schnorr::Signature::try_from(signature_slice).expect("Invalid signature");

    match verifying_key.verify_raw(msg_slice, &signature) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

fn get_master_bip32_key() -> XPrv {
    XPrv::new(&DEFAULT_SEED).expect("Failed to create master key from seed")
}

// custom master seed used in Vanadium's version of SLIP-21 for compatibility with Bolos
const SEED_MASTER_PATH: &'static str = "VANADIUM";
fn slip21_custom_get_seed() -> [u8; 32] {
    let m = slip21_get_master_node(&DEFAULT_SEED);
    let c = slip21_derive_child_node(&m, SEED_MASTER_PATH.as_bytes());
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&c[32..64]);
    seed
}

fn slip21_get_master_node(seed: &[u8]) -> [u8; 64] {
    // compute HMAC-SHA512(key = SLIP21_MAGIC, msg = seed)
    let mut mac = Hmac::<Sha512>::new_from_slice(SLIP21_MAGIC.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(seed);
    mac.finalize().into_bytes().into()
}

fn slip21_derive_child_node(cur_node: &[u8; 64], label: &[u8]) -> [u8; 64] {
    // compute HMAC-SHA512(key = cur_node[:32], msg = [0] + label)
    let mut mac =
        Hmac::<Sha512>::new_from_slice(&cur_node[..32]).expect("HMAC can take key of any size");
    mac.update(&[0u8]);
    mac.update(label);
    mac.finalize().into_bytes().into()
}

fn get_storage_file() -> String {
    // Allow overriding the storage file path via environment variable,
    // which is useful for test isolation (each test instance gets its own file).
    if let Ok(path) = std::env::var("VAPP_STORAGE_FILE") {
        return path;
    }

    let file_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .expect("Failed to compute name of storage file");

    format!("{}.dat", file_name)
}

pub fn storage_read(slot_index: u32, buffer: *mut u8, buffer_size: usize) -> u32 {
    if buffer_size != common::constants::STORAGE_SLOT_SIZE {
        eprintln!(
            "storage_read: buffer_size must be {}, got {}",
            common::constants::STORAGE_SLOT_SIZE,
            buffer_size
        );
        return 0;
    }

    if slot_index >= common::constants::MAX_STORAGE_SLOTS {
        eprintln!(
            "storage_read: slot_index {} exceeds maximum {}",
            slot_index,
            common::constants::MAX_STORAGE_SLOTS
        );
        return 0;
    }

    let storage_file = get_storage_file();
    let offset = (slot_index as u64) * (common::constants::STORAGE_SLOT_SIZE as u64);

    let data = match std::fs::File::open(&storage_file) {
        Ok(mut file) => {
            use std::io::{Read, Seek, SeekFrom};

            // Seek to the slot position
            if file.seek(SeekFrom::Start(offset)).is_err() {
                // If seek fails, return zeros
                [0u8; STORAGE_SLOT_SIZE]
            } else {
                let mut data = [0u8; STORAGE_SLOT_SIZE];
                // Read the slot data, or return zeros if read fails or is incomplete
                match file.read_exact(&mut data) {
                    Ok(_) => data,
                    Err(_) => [0u8; STORAGE_SLOT_SIZE],
                }
            }
        }
        Err(_) => {
            // File doesn't exist, return zeros
            [0u8; STORAGE_SLOT_SIZE]
        }
    };

    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), buffer, STORAGE_SLOT_SIZE);
    }

    1
}

pub fn storage_write(slot_index: u32, buffer: *const u8, buffer_size: usize) -> u32 {
    if buffer_size != common::constants::STORAGE_SLOT_SIZE {
        eprintln!(
            "storage_write: buffer_size must be {}, got {}",
            common::constants::STORAGE_SLOT_SIZE,
            buffer_size
        );
        return 0;
    }

    if slot_index >= common::constants::MAX_STORAGE_SLOTS {
        eprintln!(
            "storage_write: slot_index {} exceeds maximum {}",
            slot_index,
            common::constants::MAX_STORAGE_SLOTS
        );
        return 0;
    }

    let mut data = [0u8; STORAGE_SLOT_SIZE];
    unsafe {
        std::ptr::copy_nonoverlapping(buffer, data.as_mut_ptr(), STORAGE_SLOT_SIZE);
    }

    let storage_file = get_storage_file();
    let offset = (slot_index as u64) * (common::constants::STORAGE_SLOT_SIZE as u64);

    use std::io::{Seek, SeekFrom, Write};

    // Open file for reading and writing, create if doesn't exist
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&storage_file)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("storage_write: failed to open file {}: {}", storage_file, e);
            return 0;
        }
    };

    // Get file size
    let file_size = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            eprintln!("storage_write: failed to get file metadata: {}", e);
            return 0;
        }
    };

    // If file is smaller than the required offset + slot size, extend it with zeros
    let required_size = offset + (common::constants::STORAGE_SLOT_SIZE as u64);
    if file_size < required_size {
        if file.seek(SeekFrom::End(0)).is_err() {
            eprintln!("storage_write: failed to seek to end of file");
            return 0;
        }
        let padding = vec![0u8; (required_size - file_size) as usize];
        if file.write_all(&padding).is_err() {
            eprintln!("storage_write: failed to extend file with zeros");
            return 0;
        }
    }

    // Seek to the slot position
    if file.seek(SeekFrom::Start(offset)).is_err() {
        eprintln!("storage_write: failed to seek to slot offset");
        return 0;
    }

    // Write the slot data
    if file.write_all(&data).is_err() {
        eprintln!("storage_write: failed to write slot data");
        return 0;
    }

    // Ensure data is written to disk
    if file.flush().is_err() {
        eprintln!("storage_write: failed to flush file");
        return 0;
    }

    1
}

// Compile-time size checks: hashers must fit in the context structs
const _: () = assert!(
    std::mem::size_of::<sha2::Sha256>() <= CTX_SHA256_SIZE,
    "sha2::Sha256 does not fit in CtxSha256",
);
const _: () = assert!(
    std::mem::size_of::<sha2::Sha512>() <= CTX_SHA512_SIZE,
    "sha2::Sha512 does not fit in CtxSha512",
);
const _: () = assert!(
    std::mem::size_of::<ripemd::Ripemd160>() <= CTX_RIPEMD160_SIZE,
    "ripemd::Ripemd160 does not fit in CtxRipemd160",
);
// All sha3/keccak variants wrap the same Keccak-f[1600] state, so checking one representative
// of each family is sufficient.
const _: () = assert!(
    std::mem::size_of::<sha3::Keccak256>() <= CTX_SHA3_SIZE,
    "sha3::Keccak256 does not fit in CTX_SHA3_SIZE",
);
const _: () = assert!(
    std::mem::size_of::<sha3::Sha3_256>() <= CTX_SHA3_SIZE,
    "sha3::Sha3_256 does not fit in CTX_SHA3_SIZE",
);

pub fn hash_init(hash_identifier: u32, ctx: *mut u8) {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                panic!("hash_init: invalid output size {} for SHA-256", output_size);
            }
            let hasher = sha2::Sha256::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha256, hasher) };
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                panic!("hash_init: invalid output size {} for SHA-512", output_size);
            }
            let hasher = sha2::Sha512::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha512, hasher) };
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                panic!(
                    "hash_init: invalid output size {} for RIPEMD-160",
                    output_size
                );
            }
            let hasher = ripemd::Ripemd160::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut ripemd::Ripemd160, hasher) };
        }
        id if id == HashId::Keccak as u32 => match output_size {
            28 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak224, sha3::Keccak224::new())
            },
            32 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak256, sha3::Keccak256::new())
            },
            48 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak384, sha3::Keccak384::new())
            },
            64 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak512, sha3::Keccak512::new())
            },
            _ => panic!(
                "hash_init: invalid output size {} for Keccak (must be 28, 32, 48 or 64)",
                output_size
            ),
        },
        id if id == HashId::Sha3 as u32 => match output_size {
            28 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_224, sha3::Sha3_224::new())
            },
            32 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_256, sha3::Sha3_256::new())
            },
            48 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_384, sha3::Sha3_384::new())
            },
            64 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_512, sha3::Sha3_512::new())
            },
            _ => panic!(
                "hash_init: invalid output size {} for SHA-3 (must be 28, 32, 48 or 64)",
                output_size
            ),
        },
        _ => panic!("hash_init: unsupported hash_id {}", hash_id),
    }
}

pub fn hash_update(hash_identifier: u32, ctx: *mut u8, data: *const u8, len: usize) -> u32 {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    let data_slice = unsafe { std::slice::from_raw_parts(data, len) };
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha256) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha256, hasher) };
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha512) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha512, hasher) };
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const ripemd::Ripemd160) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut ripemd::Ripemd160, hasher) };
        }
        id if id == HashId::Keccak as u32 => match output_size {
            28 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak224) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak224, h) };
            }
            32 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak256) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak256, h) };
            }
            48 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak384) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak384, h) };
            }
            64 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak512) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak512, h) };
            }
            _ => return 0,
        },
        id if id == HashId::Sha3 as u32 => match output_size {
            28 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_224) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_224, h) };
            }
            32 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_256) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_256, h) };
            }
            48 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_384) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_384, h) };
            }
            64 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_512) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_512, h) };
            }
            _ => return 0,
        },
        _ => return 0, // Unsupported hash_id
    }
    1
}

pub fn hash_final(hash_identifier: u32, ctx: *mut u8, digest: *mut u8) -> u32 {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha256) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 32);
            }
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha512) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 64);
            }
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const ripemd::Ripemd160) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 20);
            }
        }
        id if id == HashId::Keccak as u32 => {
            macro_rules! keccak_final {
                ($ty:ty, $len:expr) => {{
                    let h = unsafe { std::ptr::read_unaligned(ctx as *const $ty) };
                    let result = h.finalize();
                    unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), digest, $len) };
                }};
            }
            match output_size {
                28 => keccak_final!(sha3::Keccak224, 28),
                32 => keccak_final!(sha3::Keccak256, 32),
                48 => keccak_final!(sha3::Keccak384, 48),
                64 => keccak_final!(sha3::Keccak512, 64),
                _ => return 0,
            }
        }
        id if id == HashId::Sha3 as u32 => {
            macro_rules! sha3_final {
                ($ty:ty, $len:expr) => {{
                    let h = unsafe { std::ptr::read_unaligned(ctx as *const $ty) };
                    let result = h.finalize();
                    unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), digest, $len) };
                }};
            }
            match output_size {
                28 => sha3_final!(sha3::Sha3_224, 28),
                32 => sha3_final!(sha3::Sha3_256, 32),
                48 => sha3_final!(sha3::Sha3_384, 48),
                64 => sha3_final!(sha3::Sha3_512, 64),
                _ => return 0,
            }
        }
        _ => return 0, // Unsupported hash_id
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::ecall_constants::{display_pack_pair, PixelFormat};

    #[test]
    fn test_display_blit_gray4() {
        // Keep PPM dumps out of the working tree during tests.
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }

        // 2x4 Gray4 image (stride 1 byte); first row: left pixel = 0xA, right = 0x5.
        let buf = [0xA5u8, 0x00, 0x00, 0x00];
        let ret = display_blit(
            display_pack_pair(0, 0),
            display_pack_pair(2, 4),
            buf.as_ptr(),
            buf.len(),
            0,
            1,
            PixelFormat::Gray4 as u32,
        );
        assert_eq!(ret, 0);

        let screen = VIRTUAL_SCREEN.lock().unwrap();
        assert_eq!(screen.pixels[0], 0xA);
        assert_eq!(screen.pixels[1], 0x5);
    }

    #[test]
    fn test_display_blit_mono1() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }

        // 8x4 Mono1 image (stride 1 byte); first row bits MSB-first:
        // 0b1000_0001 -> pixel 0 and 7 set.
        let buf = [0b1000_0001u8, 0, 0, 0];
        let ret = display_blit(
            display_pack_pair(0, 12),
            display_pack_pair(8, 4),
            buf.as_ptr(),
            buf.len(),
            0,
            1,
            PixelFormat::Mono1 as u32,
        );
        assert_eq!(ret, 0);

        let screen = VIRTUAL_SCREEN.lock().unwrap();
        let row = 12 * screen.width;
        assert_eq!(screen.pixels[row], 15);
        assert_eq!(screen.pixels[row + 1], 0);
        assert_eq!(screen.pixels[row + 7], 15);
    }

    #[test]
    fn test_display_blit_subrect_gray4() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }

        // A 4x4 Gray4 bitmap (stride 2): pixel (row, col) has intensity 4*row + col.
        let buf = [
            0x01u8, 0x23, // row 0: 0 1 2 3
            0x45, 0x67, // row 1: 4 5 6 7
            0x89, 0xAB, // row 2: 8 9 10 11
            0xCD, 0xEF, // row 3: 12 13 14 15
        ];
        // Blit the 2x4 sub-rectangle whose left edge (src_x = 1) falls mid-byte.
        let ret = display_blit(
            display_pack_pair(8, 20),
            display_pack_pair(2, 4),
            buf.as_ptr(),
            buf.len(),
            display_pack_pair(1, 0),
            2,
            PixelFormat::Gray4 as u32,
        );
        assert_eq!(ret, 0);

        let screen = VIRTUAL_SCREEN.lock().unwrap();
        for row in 0..4 {
            let base = (20 + row) * screen.width + 8;
            assert_eq!(screen.pixels[base], (4 * row + 1) as u8);
            assert_eq!(screen.pixels[base + 1], (4 * row + 2) as u8);
        }
    }

    #[test]
    fn test_display_blit_zero_stride_replicates_row() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }

        // With src_stride = 0, every output row reads the same bitmap row.
        let buf = [0x3Cu8]; // one Gray4 row: 0x3, 0xC
        let ret = display_blit(
            display_pack_pair(0, 28),
            display_pack_pair(2, 4),
            buf.as_ptr(),
            buf.len(),
            0,
            0,
            PixelFormat::Gray4 as u32,
        );
        assert_eq!(ret, 0);

        let screen = VIRTUAL_SCREEN.lock().unwrap();
        for row in 28..32 {
            assert_eq!(screen.pixels[row * screen.width], 0x3);
            assert_eq!(screen.pixels[row * screen.width + 1], 0xC);
        }
    }

    #[test]
    fn test_display_fill_rect_rgb_quantization() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }
        use common::ecall_constants::*;

        // An arbitrary (non-canonical) color quantizes to the 4-entry palette and is
        // never rejected: 0x808080 has luma 128 -> palette 2 -> intensity 10.
        assert_eq!(
            display_fill_rect(display_pack_pair(16, 36), display_pack_pair(4, 4), 0x808080),
            0
        );
        {
            let screen = VIRTUAL_SCREEN.lock().unwrap();
            assert_eq!(screen.pixels[36 * screen.width + 16], 10);
        }
        // The reserved top byte is the one invalid color encoding.
        assert_eq!(
            display_fill_rect(display_pack_pair(16, 36), display_pack_pair(4, 4), 0x0100_0000),
            DISPLAY_ERR_INVALID_ARG
        );
    }

    #[test]
    fn test_display_blit_rejects_bad_input() {
        use common::ecall_constants::*;

        let buf = [0u8; 8];
        // A 2x4 Gray4 rect at stride 1 addresses 4 bytes; 3 readable bytes is too
        // few. (A buffer larger than the rectangle needs is fine — it's a copy-rect.)
        assert_eq!(
            display_blit(
                0,
                display_pack_pair(2, 4),
                buf.as_ptr(),
                3,
                0,
                1,
                PixelFormat::Gray4 as u32,
            ),
            DISPLAY_ERR_BAD_LAYOUT
        );
        // The stride enters the requirement: at src_stride = 4 the last of the 4
        // rows ends at byte 3*4 + 1 = 13 > 8.
        assert_eq!(
            display_blit(
                0,
                display_pack_pair(2, 4),
                buf.as_ptr(),
                buf.len(),
                0,
                4,
                PixelFormat::Gray4 as u32,
            ),
            DISPLAY_ERR_BAD_LAYOUT
        );
        // Out-of-bounds rectangle.
        assert_eq!(
            display_blit(
                display_pack_pair(NATIVE_SCREEN_WIDTH as u16, 0),
                display_pack_pair(2, 4),
                buf.as_ptr(),
                4,
                0,
                1,
                PixelFormat::Gray4 as u32,
            ),
            DISPLAY_ERR_OUT_OF_BOUNDS
        );
        // y and height must respect the device's 4-row granularity.
        assert_eq!(
            display_blit(
                display_pack_pair(0, 2),
                display_pack_pair(2, 4),
                buf.as_ptr(),
                4,
                0,
                1,
                PixelFormat::Gray4 as u32,
            ),
            DISPLAY_ERR_ALIGNMENT
        );
        assert_eq!(
            display_blit(
                0,
                display_pack_pair(2, 2),
                buf.as_ptr(),
                2,
                0,
                1,
                PixelFormat::Gray4 as u32,
            ),
            DISPLAY_ERR_ALIGNMENT
        );
        // 0 is never a valid enum encoding; other unknown values may come from a
        // newer ABI revision, so they are distinguishable as UNSUPPORTED.
        assert_eq!(
            display_blit(0, display_pack_pair(2, 4), buf.as_ptr(), 4, 0, 1, 0),
            DISPLAY_ERR_INVALID_ARG
        );
        assert_eq!(
            display_blit(0, display_pack_pair(2, 4), buf.as_ptr(), 4, 0, 1, 99),
            DISPLAY_ERR_UNSUPPORTED
        );
    }

    #[test]
    fn test_slip21() {
        // testcases from https://github.com/satoshilabs/slips/blob/master/slip-0021.md
        const TEST_SEED: [u8; 64] = hex!("c76c4ac4f4e4a00d6b274d5c39c700bb4a7ddc04fbc6f78e85ca75007b5b495f74a9043eeb77bdd53aa6fc3a0e31462270316fa04b8c19114c8798706cd02ac8");
        let m = slip21_get_master_node(&TEST_SEED);
        assert_eq!(
            m[32..],
            hex!("dbf12b44133eaab506a740f6565cc117228cbf1dd70635cfa8ddfdc9af734756")
        );

        let c = slip21_derive_child_node(&m, b"SLIP-0021");
        assert_eq!(
            c[32..],
            hex!("1d065e3ac1bbe5c7fad32cf2305f7d709dc070d672044a19e610c77cdf33de0d")
        );

        assert_eq!(
            slip21_derive_child_node(&c, b"Master encryption key")[32..],
            hex!("ea163130e35bbafdf5ddee97a17b39cef2be4b4f390180d65b54cf05c6a82fde")
        );
        assert_eq!(
            slip21_derive_child_node(&c, b"Authentication key")[32..],
            hex!("47194e938ab24cc82bfa25f6486ed54bebe79c40ae2a5a32ea6db294d81861a6")
        );
    }

    macro_rules! test_hash {
        ($test_fn:ident, $ty:ty, $vectors_file:literal) => {
            #[test]
            fn $test_fn() {
                use crate::hash::Hasher as _;

                let vectors: serde_json::Value = serde_json::from_str(include_str!($vectors_file))
                    .expect("invalid test vectors JSON");

                for vector in vectors.as_array().expect("expected JSON array") {
                    let input_hex = vector["input"].as_str().expect("missing input field");
                    let expected_hex = vector["expected"].as_str().expect("missing expected field");

                    let input_bytes: Vec<u8> = if input_hex.is_empty() {
                        vec![]
                    } else {
                        hex::decode(input_hex).expect("invalid hex in input")
                    };
                    let expected_bytes =
                        hex::decode(expected_hex).expect("invalid hex in expected");

                    // Single-pass
                    let result = <$ty>::hash(&input_bytes);
                    assert_eq!(
                        &result[..],
                        expected_bytes.as_slice(),
                        "single-pass mismatch for input \"{}\"",
                        input_hex
                    );

                    // Incremental (first byte + rest) for inputs longer than 1 byte
                    if input_bytes.len() > 1 {
                        let mut hasher = <$ty>::new();
                        hasher.update(&input_bytes[..1]);
                        hasher.update(&input_bytes[1..]);
                        let result = hasher.finalize();
                        assert_eq!(
                            &result[..],
                            expected_bytes.as_slice(),
                            "incremental mismatch for input \"{}\"",
                            input_hex
                        );
                    }

                    // Incremental: one byte at a time
                    if input_bytes.len() > 1 {
                        let mut hasher = <$ty>::new();
                        for byte in &input_bytes {
                            hasher.update(&[*byte]);
                        }
                        let result = hasher.finalize();
                        assert_eq!(
                            &result[..],
                            expected_bytes.as_slice(),
                            "incremental (byte-by-byte) mismatch for input \"{}\"",
                            input_hex
                        );
                    }
                }
            }
        };
    }

    test_hash!(
        test_hash_sha256,
        crate::hash::Sha256,
        "../test-vectors/hashes/sha256.json"
    );
    test_hash!(
        test_hash_sha512,
        crate::hash::Sha512,
        "../test-vectors/hashes/sha512.json"
    );
    test_hash!(
        test_hash_ripemd160,
        crate::hash::Ripemd160,
        "../test-vectors/hashes/ripemd160.json"
    );
    test_hash!(
        test_hash_sha3_224,
        crate::hash::Sha3_224,
        "../test-vectors/hashes/sha3_224.json"
    );
    test_hash!(
        test_hash_sha3_256,
        crate::hash::Sha3_256,
        "../test-vectors/hashes/sha3_256.json"
    );
    test_hash!(
        test_hash_sha3_384,
        crate::hash::Sha3_384,
        "../test-vectors/hashes/sha3_384.json"
    );
    test_hash!(
        test_hash_sha3_512,
        crate::hash::Sha3_512,
        "../test-vectors/hashes/sha3_512.json"
    );
    test_hash!(
        test_hash_keccak224,
        crate::hash::Keccak224,
        "../test-vectors/hashes/keccak224.json"
    );
    test_hash!(
        test_hash_keccak256,
        crate::hash::Keccak256,
        "../test-vectors/hashes/keccak256.json"
    );
    test_hash!(
        test_hash_keccak384,
        crate::hash::Keccak384,
        "../test-vectors/hashes/keccak384.json"
    );
    test_hash!(
        test_hash_keccak512,
        crate::hash::Keccak512,
        "../test-vectors/hashes/keccak512.json"
    );
}
