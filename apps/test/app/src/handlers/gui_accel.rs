//! An **accelerated** counterpart to the kolibri demo (`handlers/kolibri.rs`).
//!
//! It draws the same kind of UI — title, a button row with a counter, a checkbox and a
//! slider — but entirely through the command-stream [`Screen`] ops (`fill_rect` /
//! `draw_text`), which the VM forwards to the device's native NBGL drawing. There is **no
//! guest framebuffer and no per-pixel rasterization**, so it renders in a single panel
//! refresh — orders of magnitude faster than the kolibri version, which rasterizes every
//! widget pixel in the interpreter and blits a full 144 KB framebuffer.
//!
//! This is essentially how NBGL itself draws (coarse native fills + native fonts), so the
//! performance is in the same ballpark. The trade-off is the 4-color palette and
//! square (not rounded) corners — there is no linkable rounded-rect syscall yet.
//!
//! Note: the native/emulator backend does not rasterize fonts, so on native the text is
//! blank (only the fills show); on device / Speculos the OS fonts render the text.

use alloc::{vec, vec::Vec};

use sdk::ux::screen::{Color, Font, Screen};

/// Draws a flat, bordered button (black border, light-gray face) with a centered 1–2
/// character label, using only accelerated fills + native text.
fn button(s: &Screen, x: u16, y: u16, w: u16, h: u16, label: &str) -> bool {
    let mut ok = s.fill_rect(x, y, w, h, Color::Black); // border
    ok &= s.fill_rect(x + 2, y + 2, w.saturating_sub(4), h.saturating_sub(4), Color::LightGray);
    // Rough centering for short labels (no text-measurement op exposed).
    let tx = x + w / 2 - 5;
    ok &= s.draw_text(tx, y + h / 2 - 12, 24, 28, label, Font::Bold, Color::Black);
    ok
}

/// Renders an accelerated GUI and refreshes the panel once.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)` (same shape as the other demos).
pub fn handle_gui_accel(_data: &[u8]) -> Vec<u8> {
    let s = Screen::new();
    let (w, h) = (s.width(), s.height());
    let mut ok = true;

    // White background.
    ok &= s.clear(Color::White);

    // Title + subtitle (native OS fonts).
    ok &= s.draw_text(10, 10, w - 20, 40, "Vanadium + accelerated GUI", Font::Large, Color::Black);
    ok &= s.draw_text(
        10,
        54,
        w - 20,
        24,
        "fill_rect + draw_text (native, 1 refresh)",
        Font::Regular,
        Color::Black,
    );

    // Button row with a counter.
    let by = 96;
    ok &= button(&s, 10, by, 52, 44, "-");
    ok &= s.draw_text(74, by + 12, 140, 24, "counter: 0", Font::Regular, Color::Black);
    ok &= button(&s, 210, by, 52, 44, "+");

    // Checkbox (drawn "checked": filled inner square).
    let cy = 160;
    ok &= s.fill_rect(10, cy, 36, 36, Color::Black);
    ok &= s.fill_rect(12, cy + 2, 32, 32, Color::White);
    ok &= s.fill_rect(17, cy + 7, 22, 22, Color::Black);
    ok &= s.draw_text(56, cy + 6, w - 66, 24, "enabled", Font::Regular, Color::Black);

    // Slider: a thin track with a handle at ~42%.
    let sy = 220;
    ok &= s.fill_rect(10, sy + 8, w - 20, 4, Color::DarkGray);
    let handle_x = 10 + ((w - 20) as u32 * 42 / 100) as u16;
    ok &= s.fill_rect(handle_x, sy, 12, 20, Color::Black);
    ok &= s.draw_text(10, sy + 28, w - 20, 24, "level", Font::Regular, Color::Black);

    // One panel refresh shows the whole frame.
    ok &= s.refresh();

    let mut resp = vec![];
    resp.extend_from_slice(&w.to_be_bytes());
    resp.extend_from_slice(&h.to_be_bytes());
    resp.push(ok as u8);
    resp
}
