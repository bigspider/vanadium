use alloc::{vec, vec::Vec};

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::{Gray4, GrayColor},
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Text,
};
use sdk::ux::canvas::{self, Canvas};

/// Draws a test pattern and blits it to the screen, adapting to the screen size.
///
/// On large screens it renders **band by band** with `render_banded`, so it never
/// allocates a full-screen framebuffer (which would be paged from the host and very
/// slow); the closure draws the whole scene in absolute coordinates once per band.
/// On the small Nano screens the whole framebuffer fits in the page cache, so a
/// plain `Canvas` + `flush` is used.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)`.
pub fn handle_draw(_data: &[u8]) -> Vec<u8> {
    let (w, h) = canvas::device_screen_size();
    let (wi, hi) = (w as i32, h as i32);
    let white = Gray4::WHITE.luma();

    let ok = if w >= 200 {
        // Large screen (Stax / Flex / Apex): banded rendering, single refresh.
        canvas::render_banded_for_device(white, |t| draw_large_pattern(t, wi, hi))
    } else {
        // Small monochrome screen (Nano S+ / Nano X): a full Canvas fits the cache.
        let mut c = Canvas::new_for_device();
        c.clear(white);
        draw_small_pattern(&mut c, wi, hi);
        c.flush()
    };

    let mut resp = vec![];
    resp.extend_from_slice(&(w as u16).to_be_bytes());
    resp.extend_from_slice(&(h as u16).to_be_bytes());
    resp.push(ok as u8);
    resp
}

/// Full test pattern for large screens. Drawn (possibly clipped) once per band; the
/// target is already cleared to the background, so this only adds shapes.
fn draw_large_pattern<D>(t: &mut D, w: i32, h: i32)
where
    D: DrawTarget<Color = Gray4>,
    D::Error: core::fmt::Debug,
{
    let black = Gray4::new(0);

    // Black border.
    Rectangle::new(Point::new(0, 0), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(black, 3))
        .draw(t)
        .unwrap();

    // A horizontal grayscale gradient: 16 bars from black (0) to white (15).
    let bar_w = (w - 20) / 16;
    for level in 0..16u8 {
        Rectangle::new(
            Point::new(10 + level as i32 * bar_w, 20),
            Size::new(bar_w as u32, 40),
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(level)))
        .draw(t)
        .unwrap();
    }

    // A couple of crossing diagonal lines.
    Line::new(Point::new(10, 80), Point::new(w - 10, h / 2))
        .into_styled(PrimitiveStyle::with_stroke(black, 2))
        .draw(t)
        .unwrap();
    Line::new(Point::new(w - 10, 80), Point::new(10, h / 2))
        .into_styled(PrimitiveStyle::with_stroke(black, 2))
        .draw(t)
        .unwrap();

    // A circle.
    Circle::new(Point::new(w / 2 - 40, h / 2 + 20), 80)
        .into_styled(PrimitiveStyle::with_stroke(black, 3))
        .draw(t)
        .unwrap();

    // Some text.
    Text::new(
        "Vanadium",
        Point::new(20, h - 40),
        MonoTextStyle::new(&FONT_10X20, black),
    )
    .draw(t)
    .unwrap();
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
