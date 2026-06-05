use alloc::{vec::Vec, vec};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::{Gray4, GrayColor},
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Text,
};
use sdk::ux::canvas::Canvas;

/// Draws a test pattern onto a device-sized canvas using embedded-graphics and
/// blits it to the screen. The pattern adapts to the screen size, so it works on
/// both the large touch screens (Gray4) and the small Nano screens (Mono1).
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)`.
///
/// The drawing stays on screen until the next command is processed (the test
/// V-App does not redraw its idle screen between commands).
pub fn handle_draw(_data: &[u8]) -> Vec<u8> {
    let mut canvas = Canvas::new_for_device();
    let w = canvas.width() as i32;
    let h = canvas.height() as i32;

    let black = Gray4::new(0);
    let white = Gray4::WHITE;

    // White background.
    canvas.clear(white.luma());

    if w >= 200 {
        // Large screen (Stax / Flex / Apex): full test pattern.

        // Black border.
        Rectangle::new(Point::new(0, 0), Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_stroke(black, 3))
            .draw(&mut canvas)
            .unwrap();

        // A horizontal grayscale gradient: 16 bars from black (0) to white (15).
        let bar_w = (w - 20) / 16;
        for level in 0..16u8 {
            Rectangle::new(
                Point::new(10 + level as i32 * bar_w, 20),
                Size::new(bar_w as u32, 40),
            )
            .into_styled(PrimitiveStyle::with_fill(Gray4::new(level)))
            .draw(&mut canvas)
            .unwrap();
        }

        // A couple of crossing diagonal lines.
        Line::new(Point::new(10, 80), Point::new(w - 10, h / 2))
            .into_styled(PrimitiveStyle::with_stroke(black, 2))
            .draw(&mut canvas)
            .unwrap();
        Line::new(Point::new(w - 10, 80), Point::new(10, h / 2))
            .into_styled(PrimitiveStyle::with_stroke(black, 2))
            .draw(&mut canvas)
            .unwrap();

        // A circle.
        Circle::new(Point::new(w / 2 - 40, h / 2 + 20), 80)
            .into_styled(PrimitiveStyle::with_stroke(black, 3))
            .draw(&mut canvas)
            .unwrap();

        // Some text.
        Text::new(
            "Vanadium",
            Point::new(20, h - 40),
            MonoTextStyle::new(&FONT_10X20, black),
        )
        .draw(&mut canvas)
        .unwrap();
    } else {
        // Small monochrome screen (Nano S+ / Nano X, 128x64): a compact pattern that
        // exercises shapes, lines and text within the tight bounds.

        // Border.
        Rectangle::new(Point::new(0, 0), Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_stroke(black, 1))
            .draw(&mut canvas)
            .unwrap();

        // Title text.
        Text::new(
            "Vanadium",
            Point::new(6, 12),
            MonoTextStyle::new(&FONT_6X10, black),
        )
        .draw(&mut canvas)
        .unwrap();

        // Divider under the title.
        Line::new(Point::new(4, 17), Point::new(w - 5, 17))
            .into_styled(PrimitiveStyle::with_stroke(black, 1))
            .draw(&mut canvas)
            .unwrap();

        // A filled square, an outlined circle, and a small cross.
        Rectangle::new(Point::new(8, 28), Size::new(20, 20))
            .into_styled(PrimitiveStyle::with_fill(black))
            .draw(&mut canvas)
            .unwrap();
        Circle::new(Point::new(54, 28), 20)
            .into_styled(PrimitiveStyle::with_stroke(black, 1))
            .draw(&mut canvas)
            .unwrap();
        Line::new(Point::new(96, 28), Point::new(120, 50))
            .into_styled(PrimitiveStyle::with_stroke(black, 1))
            .draw(&mut canvas)
            .unwrap();
        Line::new(Point::new(120, 28), Point::new(96, 50))
            .into_styled(PrimitiveStyle::with_stroke(black, 1))
            .draw(&mut canvas)
            .unwrap();
    }

    let ok = canvas.flush();

    let mut resp = vec![];
    resp.extend_from_slice(&(canvas.width() as u16).to_be_bytes());
    resp.extend_from_slice(&(canvas.height() as u16).to_be_bytes());
    resp.push(ok as u8);
    resp
}
