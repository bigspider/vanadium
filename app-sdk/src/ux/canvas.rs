//! A simple in-RAM framebuffer (`Canvas`) for low-level graphics.
//!
//! A `Canvas` owns a pixel buffer in guest memory. All drawing happens locally
//! (with zero ECALL overhead); only [`Canvas::flush`] / [`Canvas::flush_area`]
//! cross the ECALL boundary, blitting pixels to the device screen via the
//! `display_blit` ECALL.
//!
//! When the `embedded-graphics` feature is enabled, `Canvas` implements
//! [`embedded_graphics::draw_target::DrawTarget`] (with `Color = Gray4`) and
//! [`embedded_graphics::geometry::OriginDimensions`], so the whole
//! `embedded-graphics` ecosystem can draw onto it.

use alloc::vec;
use alloc::vec::Vec;

use common::ecall_constants::{
    PixelFormat, DEVICE_PROPERTY_PIXEL_FORMAT, DEVICE_PROPERTY_SCREEN_SIZE,
};

use crate::ecalls;

/// An in-RAM framebuffer that can be drawn into and blitted to the screen.
///
/// Pixels are stored **packed** in the target [`PixelFormat`] (4 bits per pixel
/// for `Gray4`, 1 bit for `Mono1`), so a full-screen canvas uses only
/// `stride(width) * height` bytes. This matters because the framebuffer lives in
/// the V-App heap: a full Flex screen is 144 KB packed (vs. 288 KB at one byte
/// per pixel), so graphics V-Apps should size their heap accordingly (see
/// `VAPP_HEAP_SIZE`).
///
/// Because rows are stored at the screen's stride, a full-width flush (the common
/// case, including [`Canvas::flush`]) blits the backing buffer directly with no
/// extra allocation.
pub struct Canvas {
    width: usize,
    height: usize,
    format: PixelFormat,
    stride: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    /// Creates a blank (all-zero / background) canvas of the given size and the
    /// pixel format that will be used when blitting to the screen.
    pub fn new(width: usize, height: usize, format: PixelFormat) -> Self {
        let stride = format.stride(width);
        Self {
            width,
            height,
            format,
            stride,
            pixels: vec![0u8; stride * height],
        }
    }

    /// Creates a canvas matching the current device's screen size and native
    /// pixel format, as reported by the device properties.
    pub fn new_for_device() -> Self {
        let size = ecalls::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE);
        let width = (size >> 16) as usize;
        let height = (size & 0xffff) as usize;
        let format = PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT))
            .expect("device reported an unknown pixel format");
        Self::new(width, height, format)
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    #[inline]
    pub fn height(&self) -> usize {
        self.height
    }

    #[inline]
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Fills the whole canvas with a single intensity (`0..=15`).
    pub fn clear(&mut self, intensity: u8) {
        let byte = match self.format {
            // Both nibbles set to the intensity.
            PixelFormat::Gray4 => (intensity & 0x0f) << 4 | (intensity & 0x0f),
            // All bits on or off depending on the threshold.
            PixelFormat::Mono1 => {
                if intensity >= 8 {
                    0xff
                } else {
                    0x00
                }
            }
        };
        for b in &mut self.pixels {
            *b = byte;
        }
    }

    /// Sets the intensity (`0..=15`) of a single pixel. Out-of-bounds coordinates
    /// are ignored.
    #[inline]
    pub fn set_pixel(&mut self, x: usize, y: usize, intensity: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        match self.format {
            PixelFormat::Gray4 => {
                let idx = y * self.stride + x / 2;
                let nib = intensity & 0x0f;
                let byte = &mut self.pixels[idx];
                if x % 2 == 0 {
                    *byte = (*byte & 0x0f) | (nib << 4);
                } else {
                    *byte = (*byte & 0xf0) | nib;
                }
            }
            PixelFormat::Mono1 => {
                let idx = y * self.stride + x / 8;
                let bit = 7 - (x % 8);
                if intensity >= 8 {
                    self.pixels[idx] |= 1 << bit;
                } else {
                    self.pixels[idx] &= !(1 << bit);
                }
            }
        }
    }

    /// Returns the intensity (`0..=15`) of a single pixel, or `0` if out of bounds.
    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        match self.format {
            PixelFormat::Gray4 => {
                let byte = self.pixels[y * self.stride + x / 2];
                if x % 2 == 0 {
                    byte >> 4
                } else {
                    byte & 0x0f
                }
            }
            PixelFormat::Mono1 => {
                let byte = self.pixels[y * self.stride + x / 8];
                let bit = 7 - (x % 8);
                if (byte >> bit) & 1 == 1 {
                    15
                } else {
                    0
                }
            }
        }
    }

    /// Blits the sub-rectangle `[x, x+w) × [y, y+h)` to the screen.
    ///
    /// Returns `true` on success. The rectangle must lie fully within the canvas.
    pub fn flush_area(&mut self, x: usize, y: usize, w: usize, h: usize) -> bool {
        if x + w > self.width || y + h > self.height {
            return false;
        }
        if w == 0 || h == 0 {
            return true;
        }

        // The device blit requires `y` and `height` to be multiples of 4 (an NBGL
        // constraint), so expand the region vertically to the nearest multiple of 4.
        // Harmless on the native backend. `x`/`w` have no such constraint.
        let y_end = ((y + h + 3) & !3).min(self.height);
        let y = y & !3;
        let h = y_end - y;

        // Fast path: a full-width strip is already contiguous at the canvas stride,
        // so we can blit the backing buffer directly without copying.
        if x == 0 && w == self.width {
            let start = y * self.stride;
            let end = (y + h) * self.stride;
            let buf = &self.pixels[start..end];
            let ret = unsafe {
                ecalls::display_blit(
                    0,
                    y as u32,
                    w as u32,
                    h as u32,
                    buf.as_ptr(),
                    buf.len(),
                    self.format as u32,
                )
            };
            return ret == 1;
        }

        // General path: repack the sub-rectangle at its own (narrower) stride.
        let out_stride = self.format.stride(w);
        let mut out = vec![0u8; out_stride * h];
        for row in 0..h {
            for col in 0..w {
                let intensity = self.pixel(x + col, y + row);
                match self.format {
                    PixelFormat::Gray4 => {
                        let idx = row * out_stride + col / 2;
                        if col % 2 == 0 {
                            out[idx] |= (intensity & 0x0f) << 4;
                        } else {
                            out[idx] |= intensity & 0x0f;
                        }
                    }
                    PixelFormat::Mono1 => {
                        if intensity >= 8 {
                            out[row * out_stride + col / 8] |= 1 << (7 - (col % 8));
                        }
                    }
                }
            }
        }
        let ret = unsafe {
            ecalls::display_blit(
                x as u32,
                y as u32,
                w as u32,
                h as u32,
                out.as_ptr(),
                out.len(),
                self.format as u32,
            )
        };
        ret == 1
    }

    /// Blits the entire canvas to the screen.
    pub fn flush(&mut self) -> bool {
        self.flush_area(0, 0, self.width, self.height)
    }
}

#[cfg(feature = "embedded-graphics")]
mod eg {
    use super::Canvas;
    use embedded_graphics::pixelcolor::{Gray4, GrayColor};
    use embedded_graphics::prelude::*;

    impl OriginDimensions for Canvas {
        fn size(&self) -> Size {
            Size::new(self.width as u32, self.height as u32)
        }
    }

    impl DrawTarget for Canvas {
        type Color = Gray4;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(coord, color) in pixels {
                if coord.x < 0 || coord.y < 0 {
                    continue;
                }
                self.set_pixel(coord.x as usize, coord.y as usize, color.luma());
            }
            Ok(())
        }
    }
}
