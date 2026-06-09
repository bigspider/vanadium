//! [`RefreshPolicy`]: how a damaged region is pushed to the physical panel.
//!
//! This is where each screen's refresh economics live, isolated from the widget code.
//! The reflective Stax/Flex panels are slow and ghosting-prone, so the default
//! [`EinkPolicy`] refreshes only the damaged rectangle and forces an occasional full
//! refresh to clear accumulated ghosting; a fast LCD would use [`ImmediatePolicy`].

use common::ecall_constants::PixelFormat;

use super::{Capabilities, ContentHint, Rect};
use crate::ux::screen::{RefreshMode, Screen};

/// Decides how to present a damaged rectangle. Implementations call
/// [`Screen::refresh_area`] with a device-appropriate mode and region.
pub trait RefreshPolicy {
    fn present(&mut self, screen: &Screen, dirty: Rect, hint: ContentHint);
}

// Aligns a rectangle to the panel's 4-row vertical granularity and clips it to the screen,
// returning `(x, y, w, h)` as `u16`. Returns `None` if the result is empty.
fn align4_clip(dirty: Rect, sw: i32, sh: i32) -> Option<(u16, u16, u16, u16)> {
    let r = dirty.clip(sw, sh);
    if r.is_empty() {
        return None;
    }
    let y0 = r.y & !3;
    let y1 = ((r.bottom() + 3) & !3).min(sh);
    let x0 = r.x;
    let x1 = r.right();
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16))
}

/// Refresh policy for slow, ghosting-prone reflective panels (Stax/Flex):
///
/// - refresh **only the damaged rectangle**, not the whole screen;
/// - optionally use the fast black-&-white mode for text-only changes;
/// - force a full-screen refresh every `max_partials` updates to clear ghosting.
///
/// Defaults are conservative (full-color partial refreshes); enable [`with_fast_text`]
/// after validating the black-&-white path on the target.
///
/// [`with_fast_text`]: EinkPolicy::with_fast_text
pub struct EinkPolicy {
    width: i32,
    height: i32,
    /// The panel's native pixel format. Monochrome (Mono1, the Nano panels) refreshes in
    /// black-&-white; `FullColor` is meaningless there and only the grayscale Stax/Flex
    /// panels actually have a full-color refresh.
    format: PixelFormat,
    fast_text: bool,
    partials_since_full: u32,
    max_partials: u32,
}

impl EinkPolicy {
    pub fn new(caps: &Capabilities) -> Self {
        Self {
            width: caps.size.w as i32,
            height: caps.size.h as i32,
            format: caps.pixel_format,
            fast_text: false,
            partials_since_full: 0,
            max_partials: 32,
        }
    }

    // The full-screen / full-color refresh mode appropriate for this panel.
    fn full_mode(&self) -> RefreshMode {
        match self.format {
            PixelFormat::Mono1 => RefreshMode::BlackWhite,
            PixelFormat::Gray4 => RefreshMode::FullColor,
        }
    }

    /// Use the fast black-&-white refresh mode for text-only changes (faster, but can
    /// ghost; validate on-device). Off by default.
    pub fn with_fast_text(mut self, yes: bool) -> Self {
        self.fast_text = yes;
        self
    }

    fn full_refresh(&mut self, screen: &Screen) {
        screen.refresh_area(0, 0, self.width as u16, self.height as u16, self.full_mode());
        self.partials_since_full = 0;
    }
}

impl RefreshPolicy for EinkPolicy {
    fn present(&mut self, screen: &Screen, dirty: Rect, hint: ContentHint) {
        if hint == ContentHint::FullScreen {
            self.full_refresh(screen);
            return;
        }

        // Periodically fall back to a full refresh to clear ghosting from partial updates.
        if self.partials_since_full >= self.max_partials {
            self.full_refresh(screen);
            return;
        }

        let Some((x, y, w, h)) = align4_clip(dirty, self.width, self.height) else {
            return;
        };
        let mode = if self.format == PixelFormat::Mono1 {
            // Monochrome panels have only a black-&-white refresh.
            RefreshMode::BlackWhite
        } else if self.fast_text && hint == ContentHint::Text {
            RefreshMode::BlackWhiteFast
        } else {
            RefreshMode::FullColor
        };
        screen.refresh_area(x, y, w, h, mode);
        self.partials_since_full += 1;
    }
}

/// Refresh policy for fast panels: just push the damaged rectangle, no ghosting bookkeeping.
pub struct ImmediatePolicy {
    width: i32,
    height: i32,
}

impl ImmediatePolicy {
    pub fn new(caps: &Capabilities) -> Self {
        Self {
            width: caps.size.w as i32,
            height: caps.size.h as i32,
        }
    }
}

impl RefreshPolicy for ImmediatePolicy {
    fn present(&mut self, screen: &Screen, dirty: Rect, _hint: ContentHint) {
        if let Some((x, y, w, h)) = align4_clip(dirty, self.width, self.height) {
            screen.refresh_area(x, y, w, h, RefreshMode::FullColor);
        }
    }
}
