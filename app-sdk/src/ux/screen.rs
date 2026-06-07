//! A framebuffer-less, command-stream drawing API ([`Screen`]).
//!
//! Unlike [`Canvas`](super::canvas::Canvas) — which keeps a pixel buffer in *guest*
//! RAM and blits it — `Screen` issues small *draw commands* that the VM forwards to the
//! device's native NBGL drawing, operating directly on the framebuffer the OS already
//! owns. Nothing is rasterized in the guest and only tiny descriptors (or short strings)
//! cross the ECALL boundary, so this is dramatically faster on large screens, where the
//! blit path is bottlenecked by interpreting per-pixel rasterization and paging the
//! framebuffer.
//!
//! The trade-off is expressivity: the accelerated ops cover solid rectangles and text
//! (OS fonts) in NBGL's 4-color palette. For arbitrary computed pixels or full 16-level
//! grayscale, fall back to [`Canvas`](super::canvas).
//!
//! Drawing does not touch the panel; call [`Screen::refresh`] (or
//! [`Screen::refresh_area`]) once after a batch of draw calls to make them visible.
//!
//! # Example
//! ```no_run
//! use vanadium_app_sdk::ux::screen::{Screen, Color, Font};
//!
//! let s = Screen::new();
//! s.clear(Color::White);
//! s.fill_rect(0, 0, s.width(), 4, Color::Black); // a top rule
//! s.draw_text(20, 20, s.width() - 40, 40, "Vanadium", Font::Large, Color::Black);
//! s.refresh();
//! ```

use common::ecall_constants::{
    PixelFormat, DEVICE_PROPERTY_PIXEL_FORMAT, DEVICE_PROPERTY_SCREEN_SIZE,
};

use crate::ecalls;

pub use common::ecall_constants::{Color, Font, RefreshMode};

/// The accelerated, framebuffer-less drawing surface. See the module docs.
pub struct Screen {
    width: u16,
    height: u16,
    format: PixelFormat,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    /// Creates a `Screen` for the current device, querying its geometry and native
    /// pixel format.
    pub fn new() -> Self {
        let size = ecalls::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE);
        let width = (size >> 16) as u16;
        let height = (size & 0xffff) as u16;
        let format = PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT))
            .expect("device reported an unknown pixel format");
        Self {
            width,
            height,
            format,
        }
    }

    #[inline]
    pub fn width(&self) -> u16 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Fills `[x, x+w) × [y, y+h)` with a solid palette color. Returns `false` if the
    /// rectangle falls outside the screen.
    pub fn fill_rect(&self, x: u16, y: u16, w: u16, h: u16, color: Color) -> bool {
        unsafe {
            ecalls::display_fill_rect(x as u32, y as u32, w as u32, h as u32, color as u32) == 1
        }
    }

    /// Fills the whole screen with a single color.
    pub fn clear(&self, color: Color) -> bool {
        self.fill_rect(0, 0, self.width, self.height, color)
    }

    /// Draws a UTF-8 string with an OS [`Font`] inside the given area. Text looks best
    /// over a light background (the OS anti-aliases against white).
    pub fn draw_text(
        &self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        text: &str,
        font: Font,
        color: Color,
    ) -> bool {
        let color_font = ((color as u32) << 16) | (font as u32);
        unsafe {
            ecalls::display_draw_text(
                x as u32,
                y as u32,
                w as u32,
                h as u32,
                text.as_ptr(),
                text.len(),
                color_font,
            ) == 1
        }
    }

    /// Pushes the whole screen to the panel using the device's default refresh mode
    /// (full-color on grayscale screens, black & white on monochrome ones).
    pub fn refresh(&self) -> bool {
        self.refresh_area(0, 0, self.width, self.height, self.default_refresh_mode())
    }

    /// Pushes a rectangle to the panel using the given [`RefreshMode`]. Use a partial /
    /// fast mode for small or monochrome updates to keep the (expensive) panel refresh
    /// cheap.
    pub fn refresh_area(&self, x: u16, y: u16, w: u16, h: u16, mode: RefreshMode) -> bool {
        unsafe {
            ecalls::display_refresh(x as u32, y as u32, w as u32, h as u32, mode as u32) == 1
        }
    }

    /// The sensible default refresh mode for this device's pixel format.
    pub fn default_refresh_mode(&self) -> RefreshMode {
        match self.format {
            PixelFormat::Gray4 => RefreshMode::FullColor,
            PixelFormat::Mono1 => RefreshMode::BlackWhite,
        }
    }
}
