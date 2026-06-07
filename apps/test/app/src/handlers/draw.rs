use alloc::{vec, vec::Vec};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::{Gray4, GrayColor},
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Text,
};
use sdk::ux::canvas::{self, Canvas};
use sdk::ux::screen::{Color, Font, Screen};

/// Draws a test pattern and blits it to the screen, adapting to the screen size.
///
/// On large screens it uses the **command-stream** [`Screen`] API: it issues a handful
/// of accelerated draw ops (solid fills + OS-font text) that the VM forwards straight to
/// the device framebuffer, with no guest-side framebuffer and no per-pixel rasterization.
/// On the small Nano screens, which need arbitrary monochrome pixels, it falls back to a
/// full `Canvas` (which fits the page cache) drawn with `embedded-graphics`.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)`.
pub fn handle_draw(_data: &[u8]) -> Vec<u8> {
    let (w, h) = canvas::device_screen_size();

    let ok = if w >= 200 {
        draw_large_pattern()
    } else {
        // Small monochrome screen (Nano S+ / Nano X): a full Canvas fits the cache.
        let white = Gray4::WHITE.luma();
        let mut c = Canvas::new_for_device();
        c.clear(white);
        draw_small_pattern(&mut c, w as i32, h as i32);
        c.flush()
    };

    let mut resp = vec![];
    resp.extend_from_slice(&(w as u16).to_be_bytes());
    resp.extend_from_slice(&(h as u16).to_be_bytes());
    resp.push(ok as u8);
    resp
}

/// Large-screen test pattern, drawn entirely with accelerated [`Screen`] ops and a single
/// panel refresh. No `Canvas`, no `render_banded`, no guest framebuffer.
fn draw_large_pattern() -> bool {
    let s = Screen::new();
    let (w, h) = (s.width(), s.height());
    let mut ok = true;

    // White background.
    ok &= s.clear(Color::White);

    // A black border, as four edge strips.
    let b = 4u16;
    ok &= s.fill_rect(0, 0, w, b, Color::Black); // top
    ok &= s.fill_rect(0, h - b, w, b, Color::Black); // bottom
    ok &= s.fill_rect(0, 0, b, h, Color::Black); // left
    ok &= s.fill_rect(w - b, 0, b, h, Color::Black); // right

    // The four palette colors as bars (the accelerated ops are 4-color; for 16-level
    // grayscale you'd use the Canvas/blit path instead).
    let palette = [Color::Black, Color::DarkGray, Color::LightGray, Color::White];
    let bar_w = (w - 20) / palette.len() as u16;
    for (i, &color) in palette.iter().enumerate() {
        let x = 10 + i as u16 * bar_w;
        // Outline each bar in black so the white one is visible on the white background.
        ok &= s.fill_rect(x, 20, bar_w, 40, Color::Black);
        ok &= s.fill_rect(x + 1, 21, bar_w - 2, 38, color);
    }

    // Title, rendered with an OS font (no guest-side glyph rasterization).
    ok &= s.draw_text(20, h - 64, w - 40, 40, "Vanadium", Font::Large, Color::Black);

    // A single panel refresh makes the whole frame visible.
    ok && s.refresh()
}

/// Compact test pattern for small monochrome screens (Nano, 128x64).
fn draw_small_pattern<D>(t: &mut D, w: i32, _h: i32)
where
    D: DrawTarget<Color = Gray4>,
    D::Error: core::fmt::Debug,
{
    let black = Gray4::new(0);

    Rectangle::new(Point::new(0, 0), Size::new(w as u32, _h as u32))
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(t)
        .unwrap();
    Text::new(
        "Vanadium",
        Point::new(6, 12),
        MonoTextStyle::new(&FONT_6X10, black),
    )
    .draw(t)
    .unwrap();
    Line::new(Point::new(4, 17), Point::new(w - 5, 17))
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(t)
        .unwrap();
    Rectangle::new(Point::new(8, 28), Size::new(20, 20))
        .into_styled(PrimitiveStyle::with_fill(black))
        .draw(t)
        .unwrap();
    Circle::new(Point::new(54, 28), 20)
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(t)
        .unwrap();
    Line::new(Point::new(96, 28), Point::new(120, 50))
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(t)
        .unwrap();
    Line::new(Point::new(120, 28), Point::new(96, 50))
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(t)
        .unwrap();
}
