//! Experimental example GUI built with the `kolibri-embedded-gui` immediate-mode
//! library, rendered through Vanadium's accelerated [`AcceleratedDrawTarget`] and driven
//! by real input events.
//!
//! kolibri draws (by default, with no framebuffer) directly onto its `DrawTarget`, so
//! [`AcceleratedDrawTarget`] intercepts the solid fills (`clear_background`, button/slider
//! /checkbox bodies) and turns them into native `display_fill_rect` calls, while text and
//! curves fall back to small dirty-rectangle blits. This avoids a full-screen guest
//! framebuffer entirely. (Text is still rasterized pixel-by-pixel by `embedded-graphics`
//! in the guest, so this is much faster than a full `Canvas` blit but not as fast as the
//! native-font `Screen` path.)
//!
//! ## Input
//!
//! The UI is immediate-mode: each frame we set the current [`Interaction`], rebuild the
//! widgets, and redraw. Events come from [`sdk::ux::get_event`]:
//!
//! - **Touch devices** (Stax/Flex/Apex, and the native target via synthetic input): a
//!   finger press/move/lift maps to kolibri's `Click`/`Drag`/`Release(Point)`, so the
//!   `-`/`+` buttons, checkbox and slider all react directly.
//! - **Nano** (two buttons, no pointer): kolibri has no focus model, so we map the buttons
//!   to the demo state directly — left decrements the counter, right increments it, both
//!   quit.
//!
//! The loop ends when the "Done" button is clicked (touch), both buttons are pressed
//! (Nano), or a `Quit` action arrives (e.g. EOF on the native synthetic-input stream). On
//! the native target it also auto-exits after a short idle so non-interactive runs return.

use alloc::{format, vec, vec::Vec};

use embedded_graphics::{
    geometry::{Point, Size},
    mono_font::iso_8859_10::{FONT_10X20, FONT_9X15},
    pixelcolor::Gray4,
};
use kolibri_embedded_gui::{
    button::Button,
    checkbox::Checkbox,
    label::Label,
    slider::Slider,
    style::{Spacing, Style},
    ui::{Interaction, Ui},
};
use sdk::executor::block_on;
use sdk::ux::canvas::device_screen_size;
use sdk::ux::screen_target::AcceleratedDrawTarget;
use sdk::ux::{Action, Button as HwButton, Event, PressState, TouchEvent, TouchState};

/// On the native target, exit the event loop after this many consecutive idle tickers, so
/// a non-interactive run (no `VAPP_NATIVE_INPUT`) renders the frame(s) and returns.
const NATIVE_IDLE_TICKERS: u32 = 5;

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

/// Widget-bound state that input events mutate.
struct DemoState {
    counter: i32,
    checked: bool,
    level: i16,
    done: bool,
}

/// Builds and draws one frame of the UI, applying `interaction` and mutating `state`
/// according to which widgets were touched. The `AcceleratedDrawTarget` is not refreshed
/// here; the caller pushes the frame to the panel once.
fn render_frame(
    target: &mut AcceleratedDrawTarget,
    width: u32,
    state: &mut DemoState,
    interaction: Interaction,
) {
    let counter_text = format!("counter: {}", state.counter);

    let mut ui = Ui::new_fullscreen(target, gray4_style());
    ui.interact(interaction);
    ui.clear_background().ok();

    ui.add(Label::new("Vanadium + Kolibri").with_font(FONT_10X20));
    ui.add(Label::new("immediate-mode GUI"));

    // A horizontal row: a counter flanked by buttons.
    if ui.add_horizontal(Button::new("-")).clicked() {
        state.counter -= 1;
    }
    ui.add_horizontal(Label::new(&counter_text));
    if ui.add_horizontal(Button::new("+")).clicked() {
        state.counter += 1;
    }

    // Checkbox and slider mutate `state.checked` / `state.level` internally on interaction.
    ui.add(Checkbox::new(&mut state.checked));
    ui.add(
        Slider::new(&mut state.level, 0i16..=100)
            .label("level")
            .width(width.saturating_sub(40)),
    );

    if ui.add(Button::new("Done")).clicked() {
        state.done = true;
    }
}

/// Maps a touch event to a kolibri [`Interaction`]. The seph stream reports a press (and
/// each subsequent move) as `Pressed`, and the lift as `Released`; `dragging` tracks
/// whether we are between a press and its release so moves become `Drag` rather than a new
/// `Click`.
fn map_touch(te: TouchEvent, dragging: &mut bool) -> Interaction {
    let p = Point::new(te.x as i32, te.y as i32);
    match te.state {
        TouchState::Pressed => {
            if *dragging {
                Interaction::Drag(p)
            } else {
                *dragging = true;
                Interaction::Click(p)
            }
        }
        TouchState::Released => {
            *dragging = false;
            Interaction::Release(p)
        }
    }
}

/// Renders an interactive kolibri GUI and runs its event loop until the user finishes.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)` (same shape as the `draw` demo).
pub fn handle_kolibri(_data: &[u8]) -> Vec<u8> {
    let (w, h) = device_screen_size();
    let width = w as u32;

    // Touch devices (and the native target) deliver Touch events and use the page UX model;
    // the two-button Nano devices deliver Button events instead.
    let touch_input = sdk::ux::has_page_api();
    let is_native = cfg!(feature = "target_native");

    block_on(async move {
        // Accelerated draw target: solid fills go to native display_fill_rect, the rest to
        // small dirty-rectangle blits — no full-screen guest framebuffer.
        let mut target = AcceleratedDrawTarget::new();
        let mut state = DemoState {
            counter: 0,
            checked: true,
            level: 42,
            done: false,
        };
        let mut dragging = false;

        // Initial frame.
        render_frame(&mut target, width, &mut state, Interaction::None);
        target.refresh();

        let mut idle_tickers = 0u32;
        loop {
            match sdk::ux::get_event().await {
                Event::Touch(te) if touch_input => {
                    idle_tickers = 0;
                    let interaction = map_touch(te, &mut dragging);
                    render_frame(&mut target, width, &mut state, interaction);
                    target.refresh();
                }
                // This demo reacts on *press* for instant feedback (the release-driven
                // convention is for flows where a chord must win — see nav_from_button).
                Event::Button(btn) if !touch_input => {
                    idle_tickers = 0;
                    if btn.state == PressState::Pressed {
                        match btn.button {
                            HwButton::Left => state.counter -= 1,
                            HwButton::Right => state.counter += 1,
                            HwButton::Both => state.done = true,
                        }
                    }
                    render_frame(&mut target, width, &mut state, Interaction::None);
                    target.refresh();
                }
                Event::Action(Action::Quit) => break,
                Event::Ticker => {
                    if is_native {
                        idle_tickers += 1;
                        if idle_tickers >= NATIVE_IDLE_TICKERS {
                            break;
                        }
                    }
                }
                _ => {}
            }

            if state.done {
                break;
            }
        }
    });

    let mut resp = vec![];
    resp.extend_from_slice(&(w as u16).to_be_bytes());
    resp.extend_from_slice(&(h as u16).to_be_bytes());
    resp.push(1u8);
    resp
}
