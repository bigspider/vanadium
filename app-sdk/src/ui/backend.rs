//! [`Renderer`]: the semantic draw surface, and [`ScreenRenderer`], its accelerated
//! implementation over [`Screen`].
//!
//! The renderer accumulates a **dirty bounding box** as ops are issued, so
//! [`present`](Renderer::present) refreshes only the region that actually changed —
//! delegating the *how* to a [`RefreshPolicy`].

use alloc::boxed::Box;

use common::ecall_constants::PixelFormat;

use crate::ecalls;
use crate::ux::screen::Screen;

use super::caps::{capabilities, Capabilities};
use super::refresh::{EinkPolicy, RefreshPolicy};
use super::{Align, Color, ContentHint, Font, Rect, Size};

const EMPTY: Rect = Rect::new(0, 0, 0, 0);

/// A semantic drawing surface. Backends map these operations to the device's accelerated
/// ops (native fills/text), or — only for arbitrary pixels — to a [`blit`](Renderer::blit).
/// No pixels appear in the API except that explicit escape hatch.
pub trait Renderer {
    /// The screen's capabilities.
    fn caps(&self) -> &Capabilities;

    /// The pixel size a string would occupy in `font`, for layout (no drawing).
    fn measure(&self, font: Font, text: &str) -> Size;

    /// Fills a rectangle with a solid palette color.
    fn fill_rect(&mut self, area: Rect, color: Color);

    /// Draws text within `area`, horizontally aligned per `align`. `bg` is the color behind
    /// the text, used by the OS for anti-aliasing (pass the actual background).
    fn text(&mut self, area: Rect, text: &str, font: Font, color: Color, bg: Color, align: Align);

    /// Escape hatch: blit pre-packed pixels (e.g. embedded-graphics output) into `area`.
    fn blit(&mut self, area: Rect, pixels: &[u8], format: PixelFormat);

    /// Pushes everything drawn since the last present to the panel, as one refresh of the
    /// accumulated dirty region.
    fn present(&mut self, hint: ContentHint);
}

/// A [`Renderer`] backed by the accelerated `Screen` ops and a [`RefreshPolicy`].
pub struct ScreenRenderer {
    screen: Screen,
    caps: Capabilities,
    dirty: Rect,
    policy: Box<dyn RefreshPolicy>,
}

impl ScreenRenderer {
    /// Builds a renderer for the whole device screen with the default refresh policy
    /// ([`EinkPolicy`], conservative).
    pub fn new() -> Self {
        let caps = capabilities();
        let policy = Box::new(EinkPolicy::new(&caps));
        Self {
            screen: Screen::new(),
            caps,
            dirty: EMPTY,
            policy,
        }
    }

    /// Builds a renderer with a custom refresh policy.
    pub fn with_policy(policy: Box<dyn RefreshPolicy>) -> Self {
        Self {
            screen: Screen::new(),
            caps: capabilities(),
            dirty: EMPTY,
            policy,
        }
    }

    fn screen_rect(&self) -> (i32, i32) {
        (self.caps.size.w as i32, self.caps.size.h as i32)
    }

    fn mark_dirty(&mut self, area: Rect) {
        let (w, h) = self.screen_rect();
        let clipped = area.clip(w, h);
        if !clipped.is_empty() {
            self.dirty = self.dirty.union(&clipped);
        }
    }
}

impl Default for ScreenRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for ScreenRenderer {
    fn caps(&self) -> &Capabilities {
        &self.caps
    }

    fn measure(&self, font: Font, text: &str) -> Size {
        let w = unsafe { ecalls::display_text_width(font as u32, text.as_ptr(), text.len()) };
        Size::new(w, self.caps.font(font).height as u32)
    }

    fn fill_rect(&mut self, area: Rect, color: Color) {
        let (sw, sh) = self.screen_rect();
        let r = area.clip(sw, sh);
        if r.is_empty() {
            return;
        }
        self.screen
            .fill_rect(r.x as u16, r.y as u16, r.w as u16, r.h as u16, color);
        self.mark_dirty(r);
    }

    fn text(&mut self, area: Rect, text: &str, font: Font, color: Color, bg: Color, align: Align) {
        if text.is_empty() {
            return;
        }
        let tw = self.measure(font, text).w as i32;
        let th = self.caps.font(font).height as i32;
        let tx = match align {
            Align::Left => area.x,
            Align::Center => area.x + (area.w - tw) / 2,
            Align::Right => area.right() - tw,
        };
        // Vertically center the glyph box within the row.
        let ty = area.y + (area.h - th).max(0) / 2;
        let (sw, sh) = self.screen_rect();
        let draw = Rect::new(tx, ty, tw, th).clip(sw, sh);
        if draw.is_empty() {
            return;
        }
        self.screen.draw_text(
            draw.x as u16,
            draw.y as u16,
            draw.w as u16,
            draw.h as u16,
            text,
            font,
            color,
            bg,
        );
        // Mark the caller's full box dirty (it owns the background under the text).
        self.mark_dirty(area);
    }

    fn blit(&mut self, area: Rect, pixels: &[u8], format: PixelFormat) {
        let (sw, sh) = self.screen_rect();
        let r = area.clip(sw, sh);
        if r.is_empty() {
            return;
        }
        unsafe {
            ecalls::display_blit(
                r.x as u32,
                r.y as u32,
                r.w as u32,
                r.h as u32,
                pixels.as_ptr(),
                pixels.len(),
                format as u32,
            );
        }
        self.mark_dirty(r);
    }

    fn present(&mut self, hint: ContentHint) {
        if self.dirty.is_empty() {
            return;
        }
        let dirty = self.dirty;
        self.policy.present(&self.screen, dirty, hint);
        self.dirty = EMPTY;
    }
}
