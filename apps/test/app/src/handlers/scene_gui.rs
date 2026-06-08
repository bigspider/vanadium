//! Interactive demo built on the semantic [`ui`](sdk::ui) layer, as an alternative to the
//! pixel-rasterized `kolibri` demo.
//!
//! It draws the same kind of screen (title, a counter flanked by −/+ buttons, a checkbox,
//! a slider and a Done button), but:
//!
//! - **text is drawn with the hardware font** (`nbgl_drawText`) — nothing is rasterized in
//!   the interpreted guest, so the first paint is fast;
//! - the UI is a retained [`Scene`]; each event rebuilds it and [`render_diff`] repaints
//!   **only the widget that changed**, refreshing just that rectangle (not the whole panel).
//!
//! Input is capability-driven: touch screens map presses to widget hit-tests, the two
//! Nano buttons map to counter −/+ and quit.

use alloc::{format, vec, vec::Vec};

use sdk::executor::block_on;
use sdk::ui::{
    render_diff, Align, Capabilities, Color, Font, InputModel, Point, Rect, Renderer, Scene,
    ScreenRenderer,
};
use sdk::ux::{Action, ButtonEvent, Event, TouchState};

/// On native, exit after this many idle tickers so a non-interactive run returns.
const NATIVE_IDLE_TICKERS: u32 = 5;

struct State {
    counter: i32,
    checked: bool,
    level: i32, // 0..=100
    done: bool,
}

/// Fixed widget rectangles for the current screen. Computed once; touch hit-testing and
/// scene building both read them, so they stay in sync.
struct Layout {
    screen: Rect,
    title: Rect,
    subtitle: Rect,
    minus: Rect,
    counter: Rect,
    plus: Rect,
    checkbox: Rect,
    checkbox_inner: Rect,
    checkbox_label: Rect,
    track: Rect,
    track_hit: Rect,
    slider_label: Rect,
    done: Rect,
}

fn layout(caps: &Capabilities) -> Layout {
    let w = caps.size.w as i32;
    let h = caps.size.h as i32;
    let m = 10;
    // The value label changes (its width varies with the digit count), so its box must be
    // tall enough to fully cover the glyphs when cleared — otherwise a taller device font
    // leaves a strip of stale pixels. Size it from the font's actual line height.
    let line_h = (caps.font(Font::Regular).line_height as i32).max(24);
    Layout {
        screen: Rect::new(0, 0, w, h),
        title: Rect::new(0, 12, w, 28),
        subtitle: Rect::new(0, 46, w, 20),
        minus: Rect::new(m, 86, 56, 44),
        counter: Rect::new(m + 64, 86, w - 2 * (m + 64), 44),
        plus: Rect::new(w - m - 56, 86, 56, 44),
        checkbox: Rect::new(m, 150, 40, 40),
        checkbox_inner: Rect::new(m + 8, 158, 24, 24),
        checkbox_label: Rect::new(m + 56, 150, w - (m + 56), 40),
        track: Rect::new(m + 10, 222, w - 2 * (m + 10), 6),
        track_hit: Rect::new(m + 10, 208, w - 2 * (m + 10), 34),
        slider_label: Rect::new(m + 10, 250, w - 2 * (m + 10), line_h),
        done: Rect::new(m, 300, 110, 46),
    }
}

// Position of the slider handle for the current level.
fn handle_rect(l: &Layout, level: i32) -> Rect {
    let hw = 14;
    let travel = (l.track.w - hw).max(0);
    let x = l.track.x + travel * level.clamp(0, 100) / 100;
    Rect::new(x, l.track.y - 11, hw, 28)
}

/// Rebuilds the scene from the current state. The node *count and order* are fixed across
/// frames (toggles change a node's color/area, never its presence) so the index-based diff
/// stays a cheap, local update.
fn build_scene(l: &Layout, s: &State) -> Scene {
    let mut sc = Scene::new();
    let bg = Color::White;

    // 0: background
    sc.rect(l.screen, bg);
    // 1,2: title + subtitle
    sc.text(l.title, "Vanadium UI", Font::Large, Color::Black, bg, Align::Center);
    sc.text(l.subtitle, "semantic renderer", Font::Regular, Color::Black, bg, Align::Center);

    // 3,4: minus button
    sc.rect(l.minus, Color::LightGray);
    sc.text(l.minus, "-", Font::Bold, Color::Black, Color::LightGray, Align::Center);
    // 5: counter label
    sc.text(l.counter, format!("counter: {}", s.counter), Font::Regular, Color::Black, bg, Align::Center);
    // 6,7: plus button
    sc.rect(l.plus, Color::LightGray);
    sc.text(l.plus, "+", Font::Bold, Color::Black, Color::LightGray, Align::Center);

    // 8,9,10: checkbox (outer box, inner fill, label). Inner is always present; its color
    // encodes the checked state so the node count never changes.
    sc.rect(l.checkbox, Color::LightGray);
    sc.rect(l.checkbox_inner, if s.checked { Color::Black } else { Color::LightGray });
    sc.text(l.checkbox_label, "enabled", Font::Regular, Color::Black, bg, Align::Left);

    // 11,12: slider track + handle
    sc.rect(l.track, Color::LightGray);
    sc.rect(handle_rect(l, s.level), Color::Black);
    // 13: slider value label. Left-aligned so "level:" stays put as the digit count of the
    // value changes (a centered label would shift horizontally each time).
    sc.text(l.slider_label, format!("level: {}", s.level), Font::Regular, Color::Black, bg, Align::Left);

    // 14,15: Done button
    sc.rect(l.done, Color::LightGray);
    sc.text(l.done, "Done", Font::Bold, Color::Black, Color::LightGray, Align::Center);

    sc
}

// Applies a touch at `p`. Returns true if something changed and the scene should be rebuilt.
fn on_touch(l: &Layout, s: &mut State, p: Point, pressed: bool) -> bool {
    // Slider: any touch over the track sets the level. There is deliberately no latched
    // "dragging" flag — relying on a release to clear it is fragile, since a release can be
    // dropped behind a stale press during a slow redraw, which would leave the slider
    // grabbing every later touch. Re-evaluating the track area on each event avoids that.
    if l.track_hit.contains(p) {
        let travel = l.track.w.max(1);
        s.level = (((p.x - l.track.x) * 100) / travel).clamp(0, 100);
        return true;
    }
    // Buttons act on release (touch-up), so a tap can be cancelled by sliding off first.
    if pressed {
        return false;
    }
    if l.minus.contains(p) {
        s.counter -= 1;
        true
    } else if l.plus.contains(p) {
        s.counter += 1;
        true
    } else if l.checkbox.contains(p) {
        s.checked = !s.checked;
        true
    } else if l.done.contains(p) {
        s.done = true;
        true
    } else {
        false
    }
}

/// Renders the interactive semantic-UI demo and runs its event loop.
///
/// Returns `width(u16 BE) || height(u16 BE) || ok(u8)` (same shape as the other demos).
pub fn handle_scene_gui(_data: &[u8]) -> Vec<u8> {
    let is_native = cfg!(feature = "target_native");

    let (w, h) = block_on(async move {
        let mut r = ScreenRenderer::new();
        let l = layout(r.caps());
        let pointer = r.caps().input == InputModel::Pointer;
        let (w, h) = (r.caps().size.w as u16, r.caps().size.h as u16);

        let mut state = State {
            counter: 0,
            checked: true,
            level: 42,
            done: false,
        };

        // First paint.
        let mut scene = Scene::new();
        let next = build_scene(&l, &state);
        if let Some(hint) = render_diff(&scene, &next, &mut r) {
            r.present(hint);
        }
        scene = next;

        let mut idle = 0u32;
        loop {
            let changed = match sdk::ux::get_event().await {
                Event::Touch(te) if pointer => {
                    idle = 0;
                    let p = Point::new(te.x as i32, te.y as i32);
                    on_touch(&l, &mut state, p, te.state == TouchState::Pressed)
                }
                Event::Button(btn) if !pointer => {
                    idle = 0;
                    match btn {
                        ButtonEvent::LeftPress => state.counter -= 1,
                        ButtonEvent::RightPress => state.counter += 1,
                        ButtonEvent::BothPress => state.done = true,
                        _ => {}
                    }
                    true
                }
                Event::Action(Action::Quit) => break,
                Event::Ticker => {
                    if is_native {
                        idle += 1;
                        if idle >= NATIVE_IDLE_TICKERS {
                            break;
                        }
                    }
                    false
                }
                _ => false,
            };

            if changed {
                let next = build_scene(&l, &state);
                if let Some(hint) = render_diff(&scene, &next, &mut r) {
                    r.present(hint);
                }
                scene = next;
                if state.done {
                    break;
                }
            }
        }

        (w, h)
    });

    let mut resp = vec![];
    resp.extend_from_slice(&w.to_be_bytes());
    resp.extend_from_slice(&h.to_be_bytes());
    resp.push(1u8);
    resp
}
