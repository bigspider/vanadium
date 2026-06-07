//! [`AcceleratedDrawTarget`]: an `embedded-graphics` [`DrawTarget`] that takes advantage
//! of the accelerated draw ECALLs.
//!
//! Existing `embedded-graphics`-based UIs (e.g. `kolibri-embedded-gui`) draw onto a
//! [`DrawTarget`]. The usual choice, [`Canvas`](super::canvas::Canvas), keeps a full-screen
//! pixel buffer in guest RAM and blits it — slow on large screens (interpreted per-pixel
//! rendering + paging a ~144 KB framebuffer). `AcceleratedDrawTarget` instead:
//!
//! - routes **solid fills** (`fill_solid` / `clear`, which is what backgrounds, panels,
//!   buttons, sliders, etc. use) straight to the native `display_fill_rect` op — no guest
//!   pixels at all;
//! - routes **everything else** (`draw_iter`: text glyphs, curves, rounded corners) to a
//!   small **dirty-rectangle `display_blit`**, sized to the drawn pixels, so there is no
//!   full-screen framebuffer.
//!
//! This removes the full-screen framebuffer and makes fills native, which is the bulk of
//! the cost on large screens. **It does not reach NBGL speed**, because the `draw_iter`
//! pixels (notably text) are still produced one-by-one by `embedded-graphics` in the
//! interpreted guest; only their delivery to the screen is accelerated. For true
//! native-font speed, draw text directly with [`Screen::draw_text`](super::screen::Screen).
//!
//! Compositing note: `draw_iter` provides only the foreground pixels (e.g. black glyph
//! pixels), to be drawn over whatever is already on screen. Since this target keeps no
//! framebuffer, it reconstructs the background for the blit from the most recent solid
//! fill covering that region (an idea borrowed from NBGL's "last areas" tracking). UIs
//! that draw text directly on a solid fill (the common case) composite correctly.

use alloc::vec::Vec;
use core::convert::Infallible;

use common::ecall_constants::PixelFormat;
use embedded_graphics::{
    pixelcolor::{Gray4, GrayColor},
    prelude::*,
    primitives::Rectangle,
};

use super::screen::{Color, Screen};
use crate::ecalls;

// How many recent solid fills to remember for background reconstruction.
const MAX_FILLS: usize = 24;

// A remembered solid fill: inclusive screen rectangle + its Gray4 luma.
#[derive(Clone, Copy)]
struct Fill {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    luma: u8,
}

/// An `embedded-graphics` draw target backed by the accelerated draw ECALLs. See the
/// module docs. Drawing does not refresh the panel; call [`AcceleratedDrawTarget::refresh`]
/// once after a frame.
pub struct AcceleratedDrawTarget {
    screen: Screen,
    width: i32,
    height: i32,
    fills: Vec<Fill>,
    // Reused packing buffer for blit fallbacks.
    scratch: Vec<u8>,
}

impl AcceleratedDrawTarget {
    /// Creates a target covering the whole device screen.
    pub fn new() -> Self {
        let screen = Screen::new();
        let (width, height) = (screen.width() as i32, screen.height() as i32);
        Self {
            screen,
            width,
            height,
            fills: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Pushes the drawn frame to the physical panel (a single refresh).
    pub fn refresh(&self) -> bool {
        self.screen.refresh()
    }

    fn record_fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, luma: u8) {
        if self.fills.len() == MAX_FILLS {
            self.fills.remove(0);
        }
        self.fills.push(Fill {
            x0,
            y0,
            x1,
            y1,
            luma,
        });
    }

    // Background luma to use under a sparse draw covering the given (inclusive) box: the
    // most recent solid fill that contains the box's center, or white (15) if none.
    fn background_luma(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> u8 {
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        for f in self.fills.iter().rev() {
            if cx >= f.x0 && cx <= f.x1 && cy >= f.y0 && cy <= f.y1 {
                return f.luma;
            }
        }
        15 // default background: white
    }

    // Blits the inclusive box [x0,x1]x[y0,y1], filled with `bg` luma, with `pts`
    // (x, y, luma) painted on top. `y`/`height` are expanded to multiples of 4 (an NBGL
    // blit constraint); the expansion rows take the background luma.
    fn blit_box(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, bg: u8, pts: &[(i32, i32, u8)]) {
        let x0 = x0.max(0);
        let x1 = x1.min(self.width - 1);
        // Align vertically to multiples of 4 and clamp to the screen.
        let ay0 = (y0.max(0)) & !3;
        let ay1 = (((y1 + 1).min(self.height)) + 3) & !3;
        let ay1 = ay1.min(self.height);
        if x1 < x0 || ay1 <= ay0 {
            return;
        }
        let w = (x1 - x0 + 1) as usize;
        let h = (ay1 - ay0) as usize;
        let stride = PixelFormat::Gray4.stride(w);
        let bg_byte = (bg << 4) | bg;
        self.scratch.clear();
        self.scratch.resize(stride * h, bg_byte);

        for &(px, py, luma) in pts {
            let rx = (px - x0) as usize;
            let ry = (py - ay0) as usize;
            if px < x0 || px > x1 || py < ay0 || py >= ay1 {
                continue;
            }
            let idx = ry * stride + rx / 2;
            if rx % 2 == 0 {
                self.scratch[idx] = (self.scratch[idx] & 0x0f) | (luma << 4);
            } else {
                self.scratch[idx] = (self.scratch[idx] & 0xf0) | luma;
            }
        }

        unsafe {
            ecalls::display_blit(
                x0 as u32,
                ay0 as u32,
                w as u32,
                h as u32,
                self.scratch.as_ptr(),
                self.scratch.len(),
                PixelFormat::Gray4 as u32,
            );
        }
    }
}

impl Default for AcceleratedDrawTarget {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a Gray4 luma (0..=15) to the nearest 4-color palette entry for the native fill op.
fn to_palette(luma: u8) -> Color {
    match luma {
        0..=1 => Color::Black,
        2..=6 => Color::DarkGray,
        7..=12 => Color::LightGray,
        _ => Color::White,
    }
}

impl OriginDimensions for AcceleratedDrawTarget {
    fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }
}

impl DrawTarget for AcceleratedDrawTarget {
    type Color = Gray4;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        // Collect the in-bounds pixels and their bounding box.
        let mut pts: Vec<(i32, i32, u8)> = Vec::new();
        let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for Pixel(p, c) in pixels {
            if p.x < 0 || p.y < 0 || p.x >= self.width || p.y >= self.height {
                continue;
            }
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
            pts.push((p.x, p.y, c.luma()));
        }
        if pts.is_empty() {
            return Ok(());
        }
        let bg = self.background_luma(minx, miny, maxx, maxy);
        self.blit_box(minx, miny, maxx, maxy, bg, &pts);
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        // Clip to the screen.
        let x0 = area.top_left.x.max(0);
        let y0 = area.top_left.y.max(0);
        let x1 = (area.top_left.x + area.size.width as i32 - 1).min(self.width - 1);
        let y1 = (area.top_left.y + area.size.height as i32 - 1).min(self.height - 1);
        if x1 < x0 || y1 < y0 {
            return Ok(());
        }
        let luma = color.luma();
        self.record_fill(x0, y0, x1, y1, luma);
        self.screen.fill_rect(
            x0 as u16,
            y0 as u16,
            (x1 - x0 + 1) as u16,
            (y1 - y0 + 1) as u16,
            to_palette(luma),
        );
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.fills.clear();
        let luma = color.luma();
        self.record_fill(0, 0, self.width - 1, self.height - 1, luma);
        self.screen
            .fill_rect(0, 0, self.width as u16, self.height as u16, to_palette(luma));
        Ok(())
    }
}
