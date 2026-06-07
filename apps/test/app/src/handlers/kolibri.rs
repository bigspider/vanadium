//! Experimental example GUI built with the `kolibri-embedded-gui` immediate-mode
//! library, rendered through Vanadium's accelerated [`AcceleratedDrawTarget`].
//!
//! This is **render-only**: it lays out a static screen of widgets and refreshes once.
//! kolibri is interactive by nature, but driving it needs raw touch coordinates, which
//! the VM does not deliver yet (only semantic `Action`s) — so there is no input loop here.
//!
//! kolibri draws (by default, with no framebuffer) directly onto its `DrawTarget`, so
//! [`AcceleratedDrawTarget`] intercepts the solid fills (`clear_background`, button/slider
//! /checkbox bodies) and turns them into native `display_fill_rect` calls, while text and
//! curves fall back to small dirty-rectangle blits. This avoids a full-screen guest
//! framebuffer entirely. (Text is still rasterized pixel-by-pixel by `embedded-graphics`
//! in the guest, so this is much faster than a full `Canvas` blit but not as fast as the
//! native-font `Screen` path.)

use alloc::{vec, vec::Vec};

use embedded_graphics::{
    geometry::Size,
    mono_font::iso_8859_10::{FONT_10X20, FONT_9X15},
    pixelcolor::Gray4,
};
use kolibri_embedded_gui::{
    button::Button,
    checkbox::Checkbox,
    label::Label,
    slider::Slider,
    style::{Spacing, Style},
    ui::Ui,
};
use sdk::ux::canvas::device_screen_size;
use sdk::ux::screen_target::AcceleratedDrawTarget;

/// A light grayscale (`Gray4`) theme for kolibri, suited to the e-ink screens (white
/// background, black text/borders, light-gray items). kolibri ships only `Rgb565`
/// themes, but `Style` is generic over any `PixelColor`, so we build our own.
fn gray4_style() -> Style<Gray4> {
    Style {
        background_color: Gray4::new(15), // white
        border_color: Gray4::new(0),      // black
        primary_color: Gray4::new(5),
        secondary_color: Gray4::new(8),
        icon_color: Gray4::new(0),
        default_widget_height: 40,
        border_width: 2,
        default_font: FONT_9X15,
        spacing: Spacing {
            item_spacing: Size::new(8, 8),
            button_padding: Size::new(10, 6),
            default_padding: Size::new(6, 6),
            window_border_padding: Size::new(10, 10),
        },
        item_background_color: Gray4::new(12),
        highlight_item_background_color: Gray4::new(9),
        highlight_border_color: Gray4::new(0),
        highlight_border_width: 2,
        text_color: Gray4::new(0), // black
        corner_radius: 4,
    }
}

/// Renders a static kolibri GUI and blits it to the screen.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)` (same shape as the `draw` demo).
pub fn handle_kolibri(_data: &[u8]) -> Vec<u8> {
    let (w, h) = device_screen_size();

    // Accelerated draw target: solid fills go to native display_fill_rect, the rest to
    // small dirty-rectangle blits — no full-screen guest framebuffer.
    let mut target = AcceleratedDrawTarget::new();

    // Widget-bound state. With no input wired, these just show an initial position.
    let mut checked = true;
    let mut level: i16 = 42;

    {
        let mut ui = Ui::new_fullscreen(&mut target, gray4_style());
        ui.clear_background().ok();

        ui.add(Label::new("Vanadium + Kolibri").with_font(FONT_10X20));
        ui.add(Label::new("immediate-mode GUI (render-only)"));

        // A horizontal row: a counter flanked by buttons (not interactive yet).
        ui.add_horizontal(Button::new("-"));
        ui.add_horizontal(Label::new("counter: 0"));
        ui.add_horizontal(Button::new("+"));

        ui.add(Checkbox::new(&mut checked));
        ui.add(
            Slider::new(&mut level, 0i16..=100)
                .label("level")
                .width((w as u32).saturating_sub(40)),
        );
    }

    let ok = target.refresh();

    let mut resp = vec![];
    resp.extend_from_slice(&(w as u16).to_be_bytes());
    resp.extend_from_slice(&(h as u16).to_be_bytes());
    resp.push(ok as u8);
    resp
}
