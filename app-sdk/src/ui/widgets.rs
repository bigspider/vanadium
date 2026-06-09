//! A thin set of widgets and input helpers the high-level UX flows (`review_pairs`,
//! `show_confirm_reject`, `show_info`, …) are built from, once they no longer go through
//! NBGL's page/step ECALLs.
//!
//! It is deliberately small: a [`Surface`] that owns the accelerated [`ScreenRenderer`] and
//! repaints a [`Scene`]; a couple of drawing helpers ([`button`], [`draw_icon_centered`]);
//! a word-wrapper ([`wrap_lines`]) since the low-level text op draws a single line; and the
//! input mapping that replaces NBGL's semantic events — touch hit-testing on the pointer
//! devices, and the [`Nav`] mapping of the two Nano buttons.

use alloc::string::String;
use alloc::vec::Vec;

use super::icons::IconBitmap;
use super::scene::Scene;
use super::{render_diff, Align, Capabilities, Color, Font, Point, Rect, Renderer, ScreenRenderer, Size};
use crate::ux::{ButtonEvent, Event, TouchState};

/// A drawing surface for modal UX: it owns the renderer and fully repaints a [`Scene`].
///
/// Modal flows change their whole screen between steps (a new page, a confirm screen, …),
/// so each [`paint`](Surface::paint) is a full redraw rather than the incremental diff the
/// retained-scene demo uses — simpler and free of stale pixels across transitions.
pub struct Surface {
    r: ScreenRenderer,
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    pub fn new() -> Self {
        Self {
            r: ScreenRenderer::new(),
        }
    }

    pub fn caps(&self) -> &Capabilities {
        self.r.caps()
    }

    /// The full screen rectangle.
    pub fn screen(&self) -> Rect {
        let s = self.caps().size;
        Rect::new(0, 0, s.w as i32, s.h as i32)
    }

    /// Pixel width of `text` in `font` (for layout).
    pub fn measure(&self, font: Font, text: &str) -> Size {
        self.r.measure(font, text)
    }

    /// Fully repaints the screen with `scene` and refreshes the panel.
    pub fn paint(&mut self, scene: &Scene) {
        let empty = Scene::new();
        if let Some(hint) = render_diff(&empty, scene, &mut self.r) {
            self.r.present(hint);
        }
    }
}

/// Draws a flat button (light-gray fill + centered label) into `sc`.
pub fn button(sc: &mut Scene, area: Rect, label: &str, font: Font) {
    if area.is_empty() {
        return;
    }
    sc.rect(area, Color::LightGray);
    sc.text(area, label, font, Color::Black, Color::LightGray, Align::Center);
}

/// Adds an icon centered horizontally in `screen`, with its top at `top_y` rounded down to
/// the nearest multiple of 4 (the `display_blit` ECALL requires a 4-aligned `y`/height).
/// Returns the icon's rectangle (or an empty rect if it would not fit).
pub fn draw_icon_centered(sc: &mut Scene, screen: Rect, icon: &IconBitmap, top_y: i32) -> Rect {
    let x = screen.x + (screen.w - icon.w) / 2;
    let y = top_y & !3;
    let area = Rect::new(x, y, icon.w, icon.h);
    if area.is_empty() {
        return Rect::new(0, 0, 0, 0);
    }
    sc.icon(area, icon.pixels, icon.format);
    area
}

/// Greedily word-wraps `text` into lines no wider than `max_w` pixels, measuring with
/// `measure`. A single word longer than `max_w` is hard-broken by characters so a long hex
/// value (no spaces) still wraps instead of overflowing.
pub fn wrap_lines(text: &str, max_w: i32, measure: impl Fn(&str) -> i32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if max_w <= 0 {
        lines.push(text.into());
        return lines;
    }
    let mut cur = String::new();
    for word in text.split_whitespace() {
        // Hard-break a word that does not fit on a line by itself.
        if measure(word) > max_w {
            if !cur.is_empty() {
                lines.push(core::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                let mut trial = chunk.clone();
                trial.push(ch);
                if measure(&trial) > max_w && !chunk.is_empty() {
                    lines.push(core::mem::take(&mut chunk));
                    chunk.push(ch);
                } else {
                    chunk = trial;
                }
            }
            cur = chunk;
            continue;
        }
        let trial = if cur.is_empty() {
            String::from(word)
        } else {
            let mut t = cur.clone();
            t.push(' ');
            t.push_str(word);
            t
        };
        if measure(&trial) > max_w {
            lines.push(core::mem::take(&mut cur));
            cur = String::from(word);
        } else {
            cur = trial;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// A navigation intent produced by the two Nano buttons, replacing the semantic `Action`s
/// NBGL's step callback used to deliver.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Nav {
    Prev,
    Next,
    Select,
}

/// Maps a raw Nano [`ButtonEvent`] to a [`Nav`] on **release**: left → previous, right →
/// next, both → select/confirm. Presses (and anything else) yield `None`.
///
/// Acting on release — the Ledger convention — is what makes a "both buttons" gesture work:
/// the two contacts are never perfectly simultaneous, so on press the first button down would
/// fire on its own ("first one wins"). The button state machine instead accumulates the
/// pressed mask until full release and emits a single `BothRelease`, so waiting for the
/// release yields the correct intent regardless of press timing.
pub fn nav_from_button(b: ButtonEvent) -> Option<Nav> {
    match b {
        ButtonEvent::LeftRelease => Some(Nav::Prev),
        ButtonEvent::RightRelease => Some(Nav::Next),
        ButtonEvent::BothRelease => Some(Nav::Select),
        _ => None,
    }
}

/// The screen position of a touch *release*, or `None` for a press / non-touch event.
///
/// The UX flows act on release (touch-up) so a tap can be cancelled by sliding off the
/// target first — matching the old NBGL behavior.
pub fn touch_release(event: &Event) -> Option<Point> {
    match event {
        Event::Touch(te) if te.state == TouchState::Released => {
            Some(Point::new(te.x as i32, te.y as i32))
        }
        _ => None,
    }
}
