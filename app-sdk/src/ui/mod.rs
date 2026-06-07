//! A small, **semantic** UI layer for V-Apps.
//!
//! The lesson from the embedded-graphics / kolibri experiment is that a *pixel-level*
//! drawing API (`DrawTarget`) is the wrong abstraction on Vanadium: text rasterized in the
//! interpreted guest is catastrophically slow, and the host already exposes hardware
//! accelerators (`nbgl_drawText`, `nbgl_frontDrawRect`) for exactly the high-level
//! operations a pixel API throws away. So this module draws the abstraction boundary at
//! **semantic operations** instead:
//!
//! - [`Renderer`] — a backend that realizes `fill_rect` / `text` / `blit` using whatever
//!   the device accelerates, falling back to a guest blit only for arbitrary pixels.
//! - [`Capabilities`] — what a screen can do and how it likes to be refreshed; UI code
//!   branches on capabilities, never on `target_os`.
//! - [`RefreshPolicy`] — encapsulates each panel's refresh economics (slow grayscale
//!   panels: refresh only the damaged rectangle, coalesce, clear ghosting periodically).
//! - [`Scene`] — a retained list of semantic nodes; [`render_diff`](scene::render_diff)
//!   redraws only what changed and reports the minimal dirty region.
//!
//! The heavy half of a GUI (font rasterizer, display driver) already runs on the host, so
//! this layer stays small: it is a *client* for an accelerated renderer, not a renderer.
//!
//! embedded-graphics is not required here; it remains available as the [`blit`](Renderer::blit)
//! escape hatch (e.g. via [`AcceleratedDrawTarget`](crate::ux::screen_target)) for genuinely
//! arbitrary pixels such as charts.

pub mod backend;
pub mod caps;
pub mod refresh;
pub mod scene;

pub use backend::{Renderer, ScreenRenderer};
pub use caps::{capabilities, Capabilities, FontMetrics, InputModel};
pub use refresh::{EinkPolicy, ImmediatePolicy, RefreshPolicy};
pub use scene::{render_diff, Node, Scene};

pub use common::ecall_constants::{Color, Font};

/// Horizontal alignment of text within its box.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A hint about what changed, so the [`RefreshPolicy`] can pick an appropriate panel
/// refresh mode (e.g. a fast black-&-white refresh for a text-only change).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ContentHint {
    /// The whole screen was (re)drawn — e.g. the first frame.
    FullScreen,
    /// Only text changed within the dirty region.
    Text,
    /// Arbitrary graphics changed within the dirty region.
    Graphics,
}

/// An integer point in screen pixels.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A width/height pair in screen pixels.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}

/// An axis-aligned rectangle in screen pixels (top-left origin).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// True if `p` lies inside the rectangle (inclusive of the top-left edges).
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// The smallest rectangle containing both `self` and `other`. Empty rectangles are
    /// treated as "nothing" and ignored.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    /// True if the two rectangles overlap.
    pub fn intersects(&self, other: &Rect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// The overlapping rectangle of `self` and `other` (empty if they do not overlap).
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, (right - x).max(0), (bottom - y).max(0))
    }

    /// Clips the rectangle to `[0, w) × [0, h)`.
    pub fn clip(&self, w: i32, h: i32) -> Rect {
        let x0 = self.x.clamp(0, w);
        let y0 = self.y.clamp(0, h);
        let x1 = self.right().clamp(0, w);
        let y1 = self.bottom().clamp(0, h);
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }
}
