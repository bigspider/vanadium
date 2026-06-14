use lazy_static::lazy_static;
use std::{
    io::{self, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    thread::sleep,
    time::Duration,
};

#[cfg(not(feature = "test-mode"))]
use std::io::Read;

use common::{
    constants::STORAGE_SLOT_SIZE,
    ux::{Deserializable, EventCode, EventData},
    BufferType,
};

// Used by the native unit tests (hash/slip21 vectors) below.
#[cfg(test)]
use hex_literal::hex;
#[cfg(test)]
pub(crate) use crate::ecalls_crypto::{slip21_derive_child_node, slip21_get_master_node};

const TICKER_MS: u64 = 100;

// When the interactive viewer is running, xrecv polls with this timeout so an idle app
// reports "no message" instead of blocking — letting the run-loop keep processing UX.
const XRECV_IDLE_POLL_MS: u64 = 50;

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

// A small FIFO of pending input events, mirroring the VM's queue
// (`vm/src/handlers/lib/ecall/ux_handler.rs`): bounded, oldest-dropped on overflow, with
// consecutive *Pressed* touches coalesced so a mouse-drag flood cannot grow it. Tickers
// are never queued here — `get_event` synthesizes them on timeout, which keeps input
// ahead of the ticker. The single-slot predecessor dropped a release that arrived before
// the guest drained the press; a real queue delivers both, matching the device.
const EVENT_QUEUE_CAP: usize = 4;

struct EventQueue {
    buf: std::collections::VecDeque<(common::ux::EventCode, common::ux::EventData)>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::with_capacity(EVENT_QUEUE_CAP),
        }
    }

    fn push(&mut self, ev: (common::ux::EventCode, common::ux::EventData)) {
        if self.buf.len() == EVENT_QUEUE_CAP {
            // Full (the guest drains far faster than a human generates input): drop the
            // oldest to favor the most recent input.
            self.buf.pop_front();
        }
        self.buf.push_back(ev);
    }

    fn tail(&self) -> Option<(common::ux::EventCode, common::ux::EventData)> {
        self.buf.back().copied()
    }

    fn overwrite_tail(&mut self, ev: (common::ux::EventCode, common::ux::EventData)) {
        if let Some(slot) = self.buf.back_mut() {
            *slot = ev;
        }
    }

    fn pop(&mut self) -> Option<(common::ux::EventCode, common::ux::EventData)> {
        self.buf.pop_front()
    }
}

lazy_static! {
    static ref EVENT_QUEUE: Mutex<EventQueue> = Mutex::new(EventQueue::new());
    static ref TCP_CONN: Mutex<TcpStream> = Mutex::new(wait_for_client());
}

fn get_last_event() -> Option<(common::ux::EventCode, common::ux::EventData)> {
    EVENT_QUEUE.lock().expect("Event queue mutex poisoned").pop()
}

fn store_new_event(event_code: common::ux::EventCode, event_data: common::ux::EventData) {
    let mut q = EVENT_QUEUE.lock().expect("Event queue mutex poisoned");
    enqueue_event(&mut q, event_code, event_data);
}

// The queueing policy, separated from the global lock so it can be unit-tested.
// Coalesce a drag's move-flood: a new Pressed touch overwrites a queued Pressed touch
// (mid-drag only the latest finger position matters), but never across a press/release
// edge — a fast tap within one ticker window must still deliver Pressed then Released.
// Discrete button presses/releases are always kept in order. Mirrors the VM's
// store_new_event.
fn enqueue_event(
    q: &mut EventQueue,
    event_code: common::ux::EventCode,
    event_data: common::ux::EventData,
) {
    use common::ux::{EventCode, PressState};
    if event_code == EventCode::Touch {
        if let Some((tail_code, tail_data)) = q.tail() {
            // SAFETY: an event queued with code Touch holds the union's touch variant.
            let both_pressed = tail_code == EventCode::Touch
                && unsafe {
                    tail_data.touch.state == PressState::Pressed
                        && event_data.touch.state == PressState::Pressed
                };
            if both_pressed {
                q.overwrite_tail((event_code, event_data));
                return;
            }
        }
    }
    q.push((event_code, event_data));
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

    // The device's xrecv is non-blocking: it returns 0 ("no message") when idle, which is
    // what lets an app's run-loop keep processing UX between commands — ticking the
    // dashboard-cleanup timer back to the home screen and pumping the display. Mirror that
    // when the interactive viewer is running, by polling with a read timeout; otherwise
    // (headless / scripted / CI) keep the simple blocking behavior and its exact timing.
    if webui::active() {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(XRECV_IDLE_POLL_MS)));
        xrecv_polling(&mut stream, buffer, max_size)
    } else {
        xrecv_blocking(&mut stream, buffer, max_size)
    }
}

#[cfg(not(feature = "test-mode"))]
fn xrecv_blocking(stream: &mut TcpStream, buffer: *mut u8, max_size: usize) -> usize {
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
fn xrecv_polling(stream: &mut TcpStream, buffer: *mut u8, max_size: usize) -> usize {
    use std::io::ErrorKind;
    let would_block =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut);

    // Read the 4-byte length header. A timeout *before any byte arrives* means the socket
    // is idle -> report "no message". Real traffic (a command, a chunk, an ACK) arrives
    // within the timeout, so once the frame has started we read it through to the end.
    let mut len_buf = [0u8; 4];
    let mut filled = 0;
    while filled < len_buf.len() {
        match stream.read(&mut len_buf[filled..]) {
            Ok(0) => std::process::exit(0), // host disconnected: nothing left to serve
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(ref e) if would_block(e) && filled == 0 => return 0, // idle
            Err(ref e) if would_block(e) => continue, // partial header: frame is in flight
            Err(e) => panic!("TCP read failed: {e}"),
        }
    }

    let expected = u32::from_be_bytes(len_buf) as usize;
    if expected > max_size {
        panic!(
            "Peer wants to send {} bytes but caller only provided {}-byte buffer",
            expected, max_size
        );
    }

    // The payload belongs to the same frame: read it fully, waiting through idle timeouts.
    let slice = unsafe { std::slice::from_raw_parts_mut(buffer, expected) };
    let mut filled = 0;
    while filled < slice.len() {
        match stream.read(&mut slice[filled..]) {
            Ok(0) => std::process::exit(0),
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted || would_block(e) => continue,
            Err(e) => panic!("TCP read failed: {e}"),
        }
    }
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
    use common::ux::{Action, Button, TouchEvent, TouchState};
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
        // Nano buttons (a press; append "r" for the release, e.g. "l r").
        "left" | "l" => button(Button::Left, &mut it),
        "right" | "r" => button(Button::Right, &mut it),
        "both" | "b" => button(Button::Both, &mut it),
        // Quit the current custom-GUI loop.
        "q" | "quit" => {
            let mut ed = EventData::default();
            ed.action = Action::Quit;
            (EventCode::Action, ed)
        }
        _ => return None,
    };
    return Some((code, data));

    fn button(b: common::ux::Button, it: &mut std::str::SplitWhitespace) -> (EventCode, EventData) {
        use common::ux::{ButtonEvent, PressState};
        let state = match it.next() {
            Some("r" | "release" | "up") => PressState::Released,
            _ => PressState::Pressed,
        };
        let mut ed = EventData::default();
        ed.button = ButtonEvent { button: b, state };
        (EventCode::Button, ed)
    }
}

// Reads one synthetic event from stdin, for interactively driving custom GUIs (e.g. the
// scene-UI demo) on the native target. EOF is reported as a Quit action so a loop can end.
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

    // Make sure the browser viewer is up, so a V-App that idles on get_event (rather than
    // refreshing) still gets a window and can receive input.
    webui::ensure_started();

    unsafe {
        if let Some((event_code, event_data)) = get_last_event() {
            std::ptr::write(data, event_data);
            return event_code as u32;
        }
    }

    // Optional synthetic input for interactively testing custom GUIs on native. Enabled by
    // setting VAPP_NATIVE_INPUT; it takes precedence over the viewer so runs can be scripted.
    // Page-based UX stores its action before calling get_event, so this never steals input.
    if std::env::var_os("VAPP_NATIVE_INPUT").is_some() {
        if let Some((event_code, event_data)) = read_synthetic_event() {
            unsafe {
                std::ptr::write(data, event_data);
            }
            return event_code as u32;
        }
    }

    // With the viewer active, poll the queue in small steps so browser input is delivered
    // within ~10ms instead of a whole ticker period, while still ticking ~every TICKER_MS.
    if webui::active() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(TICKER_MS);
        loop {
            unsafe {
                if let Some((event_code, event_data)) = get_last_event() {
                    std::ptr::write(data, event_data);
                    return event_code as u32;
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        return EventCode::Ticker as u32;
    }

    // Headless: wait for TICKER_MS milliseconds and return a Ticker event.
    std::thread::sleep(std::time::Duration::from_millis(TICKER_MS));
    return EventCode::Ticker as u32;
}

// ===========================================================================
// Device profiles & low-level graphics (display_blit)
//
// On the native target the "screen" is a virtual framebuffer kept in memory.
// `display_refresh` dumps it to a PPM file (inspectable with no dependency) and
// pushes it to the browser viewer (see `mod webui`).
// ===========================================================================

/// A device the native backend can emulate: drives the geometry, native pixel
/// format, display granularity and feature bits the V-App observes — so
/// capability-driven app code takes the same paths it would on the corresponding
/// hardware, and contract violations are caught on the dev machine for every
/// target, not just the default one.
#[derive(Clone)]
struct DeviceProfile {
    name: &'static str,
    width: usize,
    height: usize,
    format: common::ecall_constants::PixelFormat,
    granularity: common::ecall_constants::DisplayGranularity,
    features: u32,
}

const DEVICE_PROFILES: &[DeviceProfile] = &{
    use common::ecall_constants::*;
    // All current devices share NBGL's 4-row vertical granularity and the full set
    // of drawing accelerations; they differ in geometry, color depth and input.
    const GRAN_4ROW: DisplayGranularity = DisplayGranularity { x: 1, y: 4, w: 1, h: 4 };
    const ACCEL: u32 =
        FEATURE_ACCEL_RECT | FEATURE_ACCEL_TEXT | FEATURE_PARTIAL_REFRESH | FEATURE_FAST_MONO_REFRESH;
    [
        DeviceProfile {
            name: "flex",
            width: 480,
            height: 600,
            format: PixelFormat::Gray4,
            granularity: GRAN_4ROW,
            features: FEATURE_TOUCH | ACCEL,
        },
        DeviceProfile {
            name: "stax",
            width: 400,
            height: 672,
            format: PixelFormat::Gray4,
            granularity: GRAN_4ROW,
            features: FEATURE_TOUCH | ACCEL,
        },
        DeviceProfile {
            name: "apex_p",
            width: 300,
            height: 400,
            format: PixelFormat::Mono1,
            granularity: GRAN_4ROW,
            features: FEATURE_TOUCH | ACCEL,
        },
        DeviceProfile {
            name: "nanosplus",
            width: 128,
            height: 64,
            format: PixelFormat::Mono1,
            granularity: GRAN_4ROW,
            features: FEATURE_BUTTONS | ACCEL,
        },
        DeviceProfile {
            name: "nanox",
            width: 128,
            height: 64,
            format: PixelFormat::Mono1,
            granularity: GRAN_4ROW,
            features: FEATURE_BUTTONS | ACCEL,
        },
    ]
};

struct VirtualScreen {
    width: usize,
    height: usize,
    // One byte per pixel, intensity 0..=15 (Gray4). Simpler than packing; the
    // native target is not memory-constrained. On a Mono1 profile only 0 and 15
    // are ever written (see `panel_intensity`), so the PPM shows what the panel
    // would.
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

/// Parses a `WxH` screen-size override (separator `x` or `X`) against the profile's
/// granularity. Returns a human-readable error rather than silently falling back, so a
/// typo fails loudly like an unknown `VAPP_DEVICE`.
fn parse_screen_size(
    spec: &str,
    granularity: common::ecall_constants::DisplayGranularity,
) -> Result<(usize, usize), String> {
    let (w_str, h_str) = spec
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WIDTHxHEIGHT, e.g. 400x500".to_string())?;
    let w: usize = w_str
        .trim()
        .parse()
        .map_err(|_| format!("bad width '{}'", w_str.trim()))?;
    let h: usize = h_str
        .trim()
        .parse()
        .map_err(|_| format!("bad height '{}'", h_str.trim()))?;
    if w == 0 || h == 0 {
        return Err("width and height must be nonzero".to_string());
    }
    // Packed into a u32 device property as (w << 16) | h, so each must fit in 16 bits.
    if w > u16::MAX as usize || h > u16::MAX as usize {
        return Err(format!("width and height must each be <= {}", u16::MAX));
    }
    // A full-screen blit must satisfy the panel's alignment, so the dimensions have to be
    // whole multiples of the granularity (notably height a multiple of 4).
    let (gw, gh) = (granularity.w as usize, granularity.h as usize);
    if w % gw != 0 || h % gh != 0 {
        return Err(format!(
            "width must be a multiple of {} and height a multiple of {}",
            gw, gh
        ));
    }
    Ok((w, h))
}

lazy_static! {
    // The emulated device, selected once with the `VAPP_DEVICE` env var
    // (flex | stax | apex_p | nanosplus | nanox); flex — the tooling's default
    // target — when unset. An unknown name fails loudly rather than silently
    // emulating the wrong device. `VAPP_SCREEN_SIZE=WxH` optionally overrides the
    // geometry while keeping the rest of the profile.
    static ref DEVICE_PROFILE: DeviceProfile = {
        let name = std::env::var("VAPP_DEVICE").unwrap_or_else(|_| "flex".into());
        let mut profile = DEVICE_PROFILES
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "unknown VAPP_DEVICE '{}' (expected one of: flex, stax, apex_p, nanosplus, nanox)",
                    name
                )
            })
            .clone();
        if let Some(spec) = std::env::var_os("VAPP_SCREEN_SIZE") {
            let spec = spec.to_string_lossy();
            match parse_screen_size(&spec, profile.granularity) {
                Ok((w, h)) => {
                    profile.width = w;
                    profile.height = h;
                }
                Err(e) => panic!("invalid VAPP_SCREEN_SIZE '{}': {}", spec, e),
            }
        }
        profile
    };
    static ref VIRTUAL_SCREEN: Mutex<VirtualScreen> =
        Mutex::new(VirtualScreen::new(DEVICE_PROFILE.width, DEVICE_PROFILE.height));
}

// What the profile's panel makes of a decoded source intensity: a monochrome panel
// renders the normative Gray4 -> Mono1 threshold (level >= 8 is white), mirroring
// the VM's blit conversion.
fn panel_intensity(intensity: u8) -> u8 {
    match DEVICE_PROFILE.format {
        common::ecall_constants::PixelFormat::Gray4 => intensity,
        common::ecall_constants::PixelFormat::Mono1 => {
            if intensity >= 8 {
                15
            } else {
                0
            }
        }
    }
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
    // The virtual screen has no hardware granularity, but the emulated device does:
    // enforce the profile's constraint so the violation is caught on the dev machine.
    let g = DEVICE_PROFILE.granularity;
    if x % g.x as usize != 0
        || y % g.y as usize != 0
        || w % g.w as usize != 0
        || h % g.h as usize != 0
    {
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
            // Non-native formats convert to what the panel can show, like the VM.
            screen.pixels[(y + row) * screen_width + (x + col)] = panel_intensity(intensity);
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

    // Persist a viewable copy of the whole screen (handy on its own, and the fallback when
    // the browser viewer is disabled or unreachable).
    let path = std::env::var("VAPP_SCREEN_PPM").unwrap_or_else(|_| "vapp_screen.ppm".into());
    let _ = screen.dump_ppm(&path);

    // Push the frame to the browser viewer (a no-op when headless / in tests).
    webui::present(&screen);

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

// The intensity an accelerated-path RGB888 color renders as on the profile's panel:
// the normative 4-entry palette quantization, expanded like the device's
// `EXPAND_TO_4BPP` (`(c << 2) | c`), on grayscale panels — deliberately 4 levels,
// not 16, because the accelerated ops are palette-limited on the device — and the
// normative black/white split on monochrome ones, like the VM's quantize_rgb.
fn accel_rgb_intensity(rgb: u32) -> u8 {
    use common::ecall_constants::{rgb888_is_white_mono1, rgb888_to_palette, PixelFormat};
    match DEVICE_PROFILE.format {
        PixelFormat::Gray4 => {
            let c = rgb888_to_palette(rgb);
            (c << 2) | c
        }
        PixelFormat::Mono1 => {
            if rgb888_is_white_mono1(rgb) {
                15
            } else {
                0
            }
        }
    }
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

// ===========================================================================
// Browser viewer
//
// A dependency-free, portable replacement for a native window: a tiny blocking
// HTTP server (one background thread, thread-per-connection) streams the
// framebuffer to a browser page and reads mouse/keyboard back as touch/button
// events. Works the same on Linux and macOS, needs no system libraries, and is a
// deliberate stepping stone toward a future WASM target (the frame/input JSON
// protocol and `webui/viewer.html` are reusable; only the transport changes).
//
// Endpoints: `GET /` serves the page; `GET /frame?since=N` long-polls for the next
// framebuffer version; `POST /input` delivers an event. The page computes device
// coordinates itself, so the server only maps JSON to `EventData`.
// ===========================================================================

mod webui {
    use super::{store_new_event, VirtualScreen, DEVICE_PROFILE};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, Once};
    use std::time::{Duration, Instant};

    const VIEWER_HTML: &str = include_str!("webui/viewer.html");
    const FRAME_POLL_TIMEOUT: Duration = Duration::from_millis(1000);

    // The most recently presented framebuffer (intensity 0..=15, one byte per pixel).
    // `version` is bumped on every `present`, so a long-polling client can wait for the
    // next frame by passing the version it last saw.
    struct Frame {
        w: usize,
        h: usize,
        format: u32,
        features: u32,
        pixels: Vec<u8>,
        version: u64,
    }

    lazy_static::lazy_static! {
        static ref FRAME: Mutex<Frame> = Mutex::new(Frame {
            w: 0,
            h: 0,
            format: 0,
            features: 0,
            pixels: Vec::new(),
            version: 0,
        });
    }
    static WEBUI_START: Once = Once::new();
    static WEBUI_ACTIVE: AtomicBool = AtomicBool::new(false);

    // The viewer is off in unit tests (no port binding) and when explicitly disabled with
    // VAPP_HEADLESS — e.g. on CI or for scripted, stdin-driven runs.
    fn enabled() -> bool {
        if cfg!(test) || cfg!(feature = "test-mode") {
            return false;
        }
        std::env::var_os("VAPP_HEADLESS").is_none()
    }

    fn want_addr() -> String {
        std::env::var("VAPP_GUI_ADDR").unwrap_or_else(|_| "127.0.0.1:5005".into())
    }

    pub fn active() -> bool {
        WEBUI_ACTIVE.load(Ordering::SeqCst)
    }

    /// Starts the viewer server once, lazily, on the first frame or `get_event`. Prints
    /// the URL to open. Binding failure is non-fatal: the app keeps running with PPM
    /// output only.
    pub fn ensure_started() {
        if !enabled() {
            return;
        }
        WEBUI_START.call_once(|| match bind_listener() {
            Some((listener, addr)) => {
                // A native window opens automatically only where the feature is built in
                // and the windowing event loop can run off the main thread (Linux/Windows);
                // elsewhere the browser viewer is the entry point.
                let native_window = cfg!(feature = "native-window")
                    && cfg!(any(target_os = "linux", target_os = "windows"));
                if native_window {
                    eprintln!("Opening native window (viewer also at http://{addr})");
                } else {
                    eprintln!("Viewer: open http://{addr} in a browser");
                }
                WEBUI_ACTIVE.store(true, Ordering::SeqCst);
                std::thread::Builder::new()
                    .name("vapp-webui".into())
                    .spawn(move || serve(listener))
                    .expect("failed to spawn viewer thread");
                // Open the native window (no-op where unsupported). Spawned here, on the
                // first frame of any V-App, so it does not depend on `App::run`.
                #[cfg(feature = "native-window")]
                crate::native_window::spawn(format!("http://{addr}"));
            }
            None => {
                eprintln!("Viewer: could not bind a local port; falling back to PPM output");
            }
        });
    }

    fn bind_listener() -> Option<(TcpListener, SocketAddr)> {
        // Prefer the requested address; if it is taken, fall back to an ephemeral port so
        // two concurrent V-Apps don't collide.
        let listener = TcpListener::bind(want_addr())
            .or_else(|_| TcpListener::bind("127.0.0.1:0"))
            .ok()?;
        let addr = listener.local_addr().ok()?;
        Some((listener, addr))
    }

    /// Copies the virtual framebuffer into the shared `Frame` and bumps its version. Cheap
    /// and non-blocking — the SDL-free design does all rendering in the browser.
    pub fn present(screen: &VirtualScreen) {
        ensure_started();
        if !active() {
            return;
        }
        let mut frame = FRAME.lock().expect("Frame mutex poisoned");
        frame.w = screen.width;
        frame.h = screen.height;
        frame.format = DEVICE_PROFILE.format as u32;
        frame.features = DEVICE_PROFILE.features;
        frame.pixels.clear();
        frame.pixels.extend_from_slice(&screen.pixels);
        frame.version += 1;
    }

    fn serve(listener: TcpListener) {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                // One short-lived thread per request (Connection: close). The /frame
                // long-poll blocks at most FRAME_POLL_TIMEOUT, so threads don't pile up.
                std::thread::spawn(move || {
                    let _ = handle_conn(stream);
                });
            }
        }
    }

    fn handle_conn(mut stream: TcpStream) -> std::io::Result<()> {
        let (method, path, body) = read_request(&mut stream)?;
        match (method.as_str(), route_path(&path)) {
            ("GET", "/") => respond(&mut stream, 200, "text/html; charset=utf-8", VIEWER_HTML.as_bytes()),
            ("GET", "/frame") => handle_frame(&mut stream, &path),
            ("POST", "/input") => {
                handle_input(&body);
                respond(&mut stream, 204, "text/plain", b"")
            }
            _ => respond(&mut stream, 404, "text/plain", b"not found"),
        }
    }

    // Long-poll: block until a frame newer than `since` is available, then return it as
    // JSON; on timeout return 204 so the browser simply asks again.
    fn handle_frame(stream: &mut TcpStream, path: &str) -> std::io::Result<()> {
        let since = query_param(path, "since")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let deadline = Instant::now() + FRAME_POLL_TIMEOUT;
        loop {
            {
                let frame = FRAME.lock().expect("Frame mutex poisoned");
                if frame.version > since && frame.w > 0 {
                    let json = encode_frame_json(&frame);
                    return respond(stream, 200, "application/json", json.as_bytes());
                }
            }
            if Instant::now() >= deadline {
                return respond(stream, 204, "text/plain", b"");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn encode_frame_json(frame: &Frame) -> String {
        format!(
            "{{\"version\":{},\"w\":{},\"h\":{},\"format\":{},\"features\":{},\"pixels\":\"{}\"}}",
            frame.version,
            frame.w,
            frame.h,
            frame.format,
            frame.features,
            base64_encode(&frame.pixels),
        )
    }

    fn handle_input(body: &[u8]) {
        use common::ecall_constants::{FEATURE_BUTTONS, FEATURE_TOUCH};
        use common::ux::{
            Action, Button, ButtonEvent, EventCode, EventData, PressState, TouchEvent, TouchState,
        };

        let Ok(s) = std::str::from_utf8(body) else {
            return;
        };
        let p = &*DEVICE_PROFILE;
        let is_touch = p.features & FEATURE_TOUCH != 0;
        let is_buttons = p.features & FEATURE_BUTTONS != 0;

        match json_str(s, "type").as_deref() {
            Some("touch") if is_touch => {
                let x = json_num(s, "x").unwrap_or(0.0);
                let y = json_num(s, "y").unwrap_or(0.0);
                let state = match json_str(s, "state").as_deref() {
                    Some("released") => TouchState::Released,
                    _ => TouchState::Pressed,
                };
                let (x, y) = clamp_point(x, y, p.width, p.height);
                let mut ed = EventData::default();
                ed.touch = TouchEvent::new(x, y, state);
                store_new_event(EventCode::Touch, ed);
            }
            Some("button") if is_buttons => {
                let button = match json_str(s, "button").as_deref() {
                    Some("left") => Button::Left,
                    Some("right") => Button::Right,
                    Some("both") => Button::Both,
                    _ => return,
                };
                let state = match json_str(s, "state").as_deref() {
                    Some("released") => PressState::Released,
                    _ => PressState::Pressed,
                };
                let mut ed = EventData::default();
                ed.button = ButtonEvent { button, state };
                store_new_event(EventCode::Button, ed);
            }
            // End a running demo loop without killing the process.
            Some("stop") => {
                let mut ed = EventData::default();
                ed.action = Action::Quit;
                store_new_event(EventCode::Action, ed);
            }
            // Closing the viewer exits the process, the browser analog of closing a window.
            Some("quit") => std::process::exit(0),
            _ => {}
        }
    }

    fn clamp_point(x: f64, y: f64, w: usize, h: usize) -> (u16, u16) {
        let cx = x.max(0.0).min(w.saturating_sub(1) as f64) as u16;
        let cy = y.max(0.0).min(h.saturating_sub(1) as f64) as u16;
        (cx, cy)
    }

    // --- minimal HTTP/1.1 and JSON helpers (we control both ends; not a general server) ---

    fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut tmp)?;
            if n == 0 || buf.len() > 64 * 1024 {
                // Connection closed before a full header, or an implausibly large header.
                return Ok((String::new(), String::new(), Vec::new()));
            }
            buf.extend_from_slice(&tmp[..n]);
        };

        let header = String::from_utf8_lossy(&buf[..header_end]);
        let mut lines = header.split("\r\n");
        let mut request_line = lines.next().unwrap_or("").split_whitespace();
        let method = request_line.next().unwrap_or("").to_string();
        let path = request_line.next().unwrap_or("").to_string();

        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }

        let mut body = buf[header_end + 4..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);
        Ok((method, path, body))
    }

    fn respond(
        stream: &mut TcpStream,
        status: u16,
        content_type: &str,
        body: &[u8],
    ) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            404 => "Not Found",
            _ => "OK",
        };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()
    }

    fn route_path(path: &str) -> &str {
        path.split('?').next().unwrap_or(path)
    }

    fn query_param(path: &str, key: &str) -> Option<String> {
        let query = path.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    // Extracts a flat string field `"key":"value"` from our own compact JSON.
    fn json_str(s: &str, key: &str) -> Option<String> {
        let rest = field_value(s, key)?;
        let after = rest.trim_start().strip_prefix('"')?;
        let end = after.find('"')?;
        Some(after[..end].to_string())
    }

    // Extracts a flat numeric field `"key":number` from our own compact JSON.
    fn json_num(s: &str, key: &str) -> Option<f64> {
        let after = field_value(s, key)?.trim_start();
        let end = after
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
            .unwrap_or(after.len());
        after[..end].parse().ok()
    }

    // Returns the slice just after `"key":` for a flat JSON object.
    fn field_value<'a>(s: &'a str, key: &str) -> Option<&'a str> {
        let needle = format!("\"{}\"", key);
        let start = s.find(&needle)? + needle.len();
        let colon = s[start..].find(':')?;
        Some(&s[start + colon + 1..])
    }

    fn base64_encode(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    #[cfg(test)]
    mod webui_tests {
        use super::*;

        #[test]
        fn base64_known_vectors() {
            assert_eq!(base64_encode(b""), "");
            assert_eq!(base64_encode(b"f"), "Zg==");
            assert_eq!(base64_encode(b"fo"), "Zm8=");
            assert_eq!(base64_encode(b"foo"), "Zm9v");
            assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
            assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
            assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        }

        #[test]
        fn json_field_extraction() {
            let s = r#"{"type":"touch","x":12,"y":34.5,"state":"pressed"}"#;
            assert_eq!(json_str(s, "type").as_deref(), Some("touch"));
            assert_eq!(json_str(s, "state").as_deref(), Some("pressed"));
            assert_eq!(json_num(s, "x"), Some(12.0));
            assert_eq!(json_num(s, "y"), Some(34.5));
            assert_eq!(json_str(s, "missing"), None);
        }

        #[test]
        fn query_param_parsing() {
            assert_eq!(route_path("/frame?since=7"), "/frame");
            assert_eq!(query_param("/frame?since=7", "since").as_deref(), Some("7"));
            assert_eq!(query_param("/frame", "since"), None);
        }

        #[test]
        fn clamp_keeps_point_in_bounds() {
            assert_eq!(clamp_point(-3.0, 1000.0, 128, 64), (0, 63));
            assert_eq!(clamp_point(10.9, 20.2, 128, 64), (10, 20));
        }
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

pub fn show_step(step_desc: *const u8, step_desc_len: usize) -> u32 {
    // The step UX model is what apps use on the button-device profiles
    // (VAPP_DEVICE=nanosplus|nanox); render it to the console like show_page does.
    // No interaction is synthesized: drive the flow with VAPP_NATIVE_INPUT if needed.
    let step_desc_slice = unsafe { std::slice::from_raw_parts(step_desc, step_desc_len) };
    let Ok(step) = common::ux::Step::deserialize_full(step_desc_slice) else {
        return 0;
    };

    println!("\n+=========================================+");
    match step {
        common::ux::Step::TextSubtext { text, subtext, .. } => {
            println!("{}", text);
            println!("{}", subtext);
        }
        common::ux::Step::CenteredInfo { text, subtext, icon, .. } => {
            if !matches!(icon, common::ux::Icon::None) {
                println!("[{:?}]", icon);
            }
            if let Some(text) = text {
                println!("{}", text);
            }
            if let Some(subtext) = subtext {
                println!("{}", subtext);
            }
        }
    }
    epilogue_noaction();

    1
}

pub fn get_device_property(property: u32) -> u32 {
    use common::ecall_constants::*;
    // Everything device-shaped comes from the selected `VAPP_DEVICE` profile, so a
    // capability-driven app exercises the same branches it would on that hardware.
    let p = &*DEVICE_PROFILE;
    match property {
        // The native pseudo-device: vendor 0xFFFF, product 1 (nonzero per the
        // property contract — 0 always means "unsupported property"). Deliberately
        // not per-profile: apps must branch on features, never on the id.
        DEVICE_PROPERTY_ID => 0xFFFF_0001,
        DEVICE_PROPERTY_SCREEN_SIZE => ((p.width as u32) << 16) | (p.height as u32),
        DEVICE_PROPERTY_FEATURES => p.features,
        DEVICE_PROPERTY_PIXEL_FORMAT => p.format as u32,
        DEVICE_PROPERTY_DISPLAY_GRANULARITY => p.granularity.pack(),
        DEVICE_PROPERTY_MAX_TEXT_LEN => DISPLAY_MAX_TEXT_LEN as u32,
        DEVICE_PROPERTY_ABI_REVISION => VANADIUM_ABI_REVISION,
        // Unknown properties return 0 (the probing contract), never panic.
        _ => 0,
    }
}


// ---------------------------------------------------------------------------
// Crypto / bignum / hash ECALLs are pure-Rust software implementations shared
// with the wasm backend (see `ecalls_crypto`). Randomness comes from
// `fill_random` below.
// ---------------------------------------------------------------------------
pub use crate::ecalls_crypto::{
    bn_addm, bn_modinv_prime, bn_modm, bn_multm, bn_powm, bn_subm, derive_hd_node,
    derive_slip21_node, ecdsa_sign, ecdsa_verify, ecfp_add_point, ecfp_scalar_mult,
    get_master_fingerprint, get_random_bytes, hash_final, hash_init, hash_update,
    schnorr_sign, schnorr_verify,
};

/// Randomness for the shared crypto module: the OS RNG.
pub(crate) fn fill_random(buf: &mut [u8]) {
    use rand::TryRngCore;
    rand::rngs::OsRng::default()
        .try_fill_bytes(buf)
        .expect("Failed to generate random bytes");
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
        assert_eq!(screen.pixels[0], panel_intensity(0xA));
        assert_eq!(screen.pixels[1], panel_intensity(0x5));
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
            assert_eq!(screen.pixels[base], panel_intensity((4 * row + 1) as u8));
            assert_eq!(screen.pixels[base + 1], panel_intensity((4 * row + 2) as u8));
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
            assert_eq!(screen.pixels[row * screen.width], panel_intensity(0x3));
            assert_eq!(screen.pixels[row * screen.width + 1], panel_intensity(0xC));
        }
    }

    // The properties served to the app must agree with the profile actually driving
    // the virtual screen, whatever VAPP_DEVICE selected — they are two views of the
    // same device.
    #[test]
    fn test_device_properties_match_profile() {
        use common::ecall_constants::*;
        let p = &*DEVICE_PROFILE;
        assert_eq!(
            get_device_property(DEVICE_PROPERTY_SCREEN_SIZE),
            ((p.width as u32) << 16) | p.height as u32
        );
        {
            let screen = VIRTUAL_SCREEN.lock().unwrap();
            assert_eq!((screen.width, screen.height), (p.width, p.height));
        }
        assert_eq!(get_device_property(DEVICE_PROPERTY_FEATURES), p.features);
        // Exactly one input model is advertised.
        assert_eq!(
            (p.features & FEATURE_TOUCH != 0) as u32 + (p.features & FEATURE_BUTTONS != 0) as u32,
            1
        );
        assert_eq!(
            PixelFormat::from_u32(get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT)),
            Some(p.format)
        );
        assert_eq!(
            DisplayGranularity::from_u32(get_device_property(DEVICE_PROPERTY_DISPLAY_GRANULARITY)),
            Some(p.granularity)
        );
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
            assert_eq!(screen.pixels[36 * screen.width + 16], accel_rgb_intensity(0x808080));
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
                display_pack_pair(DEVICE_PROFILE.width as u16, 0),
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
    fn test_display_refresh_codes() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }
        use common::ecall_constants::*;

        let size = display_pack_pair(4, 4);
        // Mode 0 is malformed; an unknown nonzero mode may exist on a newer VM.
        assert_eq!(display_refresh(0, size, 0), DISPLAY_ERR_INVALID_ARG);
        assert_eq!(display_refresh(0, size, 99), DISPLAY_ERR_UNSUPPORTED);
        // Modes are advisory: every defined one succeeds on every device.
        for mode in [
            RefreshMode::FullQuality,
            RefreshMode::Partial,
            RefreshMode::Mono,
            RefreshMode::MonoFast,
        ] {
            assert_eq!(display_refresh(0, size, mode as u32), 0);
        }
        // The rectangle is advisory too: empty or off-screen rects clip to a no-op
        // success — refresh never reports OUT_OF_BOUNDS.
        assert_eq!(display_refresh(0, 0, RefreshMode::FullQuality as u32), 0);
        assert_eq!(
            display_refresh(
                display_pack_pair(0xffff, 0xffff),
                display_pack_pair(8, 8),
                RefreshMode::FullQuality as u32,
            ),
            0
        );
    }

    #[test]
    fn test_display_draw_text_codes() {
        unsafe { std::env::set_var("VAPP_SCREEN_PPM", std::env::temp_dir().join("vapp_test.ppm")); }
        use common::ecall_constants::*;

        let txt = b"hello";
        let pos = display_pack_pair(0, 0);
        let size = display_pack_pair(120, 24);
        let call = |font: u32, color: u32, bg: u32| {
            display_draw_text(pos, size, txt.as_ptr(), txt.len(), font, color, bg)
        };
        // Unknown font: 0 malformed, nonzero probe-able.
        assert_eq!(call(0, 0, 0xffffff), DISPLAY_ERR_INVALID_ARG);
        assert_eq!(call(99, 0, 0xffffff), DISPLAY_ERR_UNSUPPORTED);
        // Reserved color bits, on either color (colors are otherwise never rejected).
        let font = Font::Regular as u32;
        assert_eq!(call(font, 0x0100_0000, 0xffffff), DISPLAY_ERR_INVALID_ARG);
        assert_eq!(call(font, 0, 0x0100_0000), DISPLAY_ERR_INVALID_ARG);
        // An empty clip box draws nothing, successfully (checked before bounds/text).
        assert_eq!(
            display_draw_text(pos, 0, txt.as_ptr(), txt.len(), font, 0, 0xffffff),
            0
        );
        // Box not contained in the screen.
        assert_eq!(
            display_draw_text(
                display_pack_pair(0, DEVICE_PROFILE.height as u16),
                size,
                txt.as_ptr(),
                txt.len(),
                font,
                0,
                0xffffff,
            ),
            DISPLAY_ERR_OUT_OF_BOUNDS
        );
        // Over the advertised length limit (soft — in v1 this killed the V-App).
        let long = vec![b'a'; DISPLAY_MAX_TEXT_LEN + 1];
        assert_eq!(
            display_draw_text(pos, size, long.as_ptr(), long.len(), font, 0, 0xffffff),
            DISPLAY_ERR_TOO_LONG
        );
        // Bad UTF-8 and interior NUL.
        assert_eq!(
            display_draw_text(pos, size, b"\xff\xfe".as_ptr(), 2, font, 0, 0xffffff),
            DISPLAY_ERR_INVALID_ARG
        );
        assert_eq!(
            display_draw_text(pos, size, b"a\0b".as_ptr(), 3, font, 0, 0xffffff),
            DISPLAY_ERR_INVALID_ARG
        );
        // And the happy path.
        assert_eq!(call(font, 0x000000, 0xffffff), 0);
    }

    #[test]
    fn test_display_text_width_and_font_metrics_codes() {
        use common::ecall_constants::*;

        let txt = b"abc";
        assert_eq!(
            display_text_width(0, txt.as_ptr(), txt.len()),
            DISPLAY_ERR_INVALID_ARG
        );
        assert_eq!(
            display_text_width(99, txt.as_ptr(), txt.len()),
            DISPLAY_ERR_UNSUPPORTED
        );
        let font = Font::Regular as u32;
        let long = vec![b'a'; DISPLAY_MAX_TEXT_LEN + 1];
        assert_eq!(
            display_text_width(font, long.as_ptr(), long.len()),
            DISPLAY_ERR_TOO_LONG
        );
        assert_eq!(
            display_text_width(font, b"\xff".as_ptr(), 1),
            DISPLAY_ERR_INVALID_ARG
        );
        assert_eq!(
            display_text_width(font, b"a\0b".as_ptr(), 3),
            DISPLAY_ERR_INVALID_ARG
        );
        // With the signed convention, 0 unambiguously means an empty string.
        assert_eq!(display_text_width(font, txt.as_ptr(), 0), 0);
        assert!(display_text_width(font, txt.as_ptr(), txt.len()) > 0);

        assert_eq!(display_font_metrics(0), DISPLAY_ERR_INVALID_ARG);
        assert_eq!(display_font_metrics(99), DISPLAY_ERR_UNSUPPORTED);
        let m = display_font_metrics(font);
        assert!(m > 0);
        assert!((m >> 16) > 0 && (m & 0xffff) > 0, "both packed heights nonzero");
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

    fn touch_event(x: u16, y: u16, state: common::ux::TouchState) -> common::ux::EventData {
        let mut ed = common::ux::EventData::default();
        ed.touch = common::ux::TouchEvent::new(x, y, state);
        ed
    }

    fn button_event(button: common::ux::Button) -> common::ux::EventData {
        let mut ed = common::ux::EventData::default();
        ed.button = common::ux::ButtonEvent {
            button,
            state: common::ux::PressState::Pressed,
        };
        ed
    }

    #[test]
    fn event_queue_is_fifo_and_drops_oldest_when_full() {
        use common::ux::{Button, EventCode};
        let mut q = EventQueue::new();
        // Five discrete button events into a cap-4 queue: the first (Left) is evicted.
        for b in [Button::Left, Button::Right, Button::Left, Button::Right, Button::Both] {
            enqueue_event(&mut q, EventCode::Button, button_event(b));
        }
        let drained: Vec<Button> = std::iter::from_fn(|| q.pop())
            .map(|(_, d)| unsafe { d.button.button })
            .collect();
        assert_eq!(
            drained,
            vec![Button::Right, Button::Left, Button::Right, Button::Both]
        );
    }

    #[test]
    fn touch_pressed_coalesces_but_release_does_not() {
        use common::ux::{EventCode, TouchState};
        let mut q = EventQueue::new();
        // A drag's move-flood collapses to a single Pressed with the latest coordinates.
        enqueue_event(&mut q, EventCode::Touch, touch_event(1, 1, TouchState::Pressed));
        enqueue_event(&mut q, EventCode::Touch, touch_event(2, 2, TouchState::Pressed));
        enqueue_event(&mut q, EventCode::Touch, touch_event(9, 7, TouchState::Pressed));
        assert_eq!(q.buf.len(), 1);
        let (code, data) = q.pop().unwrap();
        assert_eq!(code, EventCode::Touch);
        unsafe {
            assert_eq!((data.touch.x, data.touch.y), (9, 7));
            assert_eq!(data.touch.state, TouchState::Pressed);
        }
        // A press then a release within one window must stay two distinct events.
        enqueue_event(&mut q, EventCode::Touch, touch_event(4, 4, TouchState::Pressed));
        enqueue_event(&mut q, EventCode::Touch, touch_event(4, 4, TouchState::Released));
        assert_eq!(q.buf.len(), 2);
        let states: Vec<TouchState> = std::iter::from_fn(|| q.pop())
            .map(|(_, d)| unsafe { d.touch.state })
            .collect();
        assert_eq!(states, vec![TouchState::Pressed, TouchState::Released]);
    }

    #[test]
    fn parse_screen_size_accepts_and_rejects() {
        use common::ecall_constants::DisplayGranularity;
        let g = DisplayGranularity { x: 1, y: 4, w: 1, h: 4 };
        assert_eq!(parse_screen_size("400x500", g), Ok((400, 500)));
        assert_eq!(parse_screen_size("128X64", g), Ok((128, 64))); // uppercase separator
        assert!(parse_screen_size("0x0", g).is_err()); // zero
        assert!(parse_screen_size("400x501", g).is_err()); // height not a multiple of 4
        assert!(parse_screen_size("100000x100", g).is_err()); // exceeds u16
        assert!(parse_screen_size("400", g).is_err()); // missing separator
        assert!(parse_screen_size("axb", g).is_err()); // not numbers
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
