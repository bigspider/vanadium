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
    PixelFormat, RefreshMode, DEVICE_PROPERTY_PIXEL_FORMAT, DEVICE_PROPERTY_SCREEN_SIZE,
};

/// The sensible default panel refresh mode for a given pixel format: full-color for
/// grayscale screens, black & white for monochrome ones.
fn default_refresh_mode(format: PixelFormat) -> RefreshMode {
    match format {
        PixelFormat::Gray4 => RefreshMode::FullQuality,
        PixelFormat::Mono1 => RefreshMode::Mono,
    }
}

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
///
/// # Performance: do not full-screen `flush()` on large devices
///
/// The framebuffer lives in *guest* memory, which is paged to the host 256 bytes
/// at a time. A full Flex screen (144 KB) is ~563 pages but the VM's data cache
/// holds only ~12, so building and blitting a full-screen `Canvas` thrashes the
/// cache and is very slow. On large screens, render with [`render_banded`] (or
/// [`render_banded_raw`]) instead: it draws the scene band by band into one small,
/// cache-resident buffer, so the framebuffer never round-trips to the host. Reserve
/// a standalone `Canvas` + [`Canvas::flush_area`] for genuinely small dirty
/// rectangles (a status line, a toggled icon) and for the small Nano screens, where
/// the whole framebuffer (~1 KB) fits in the cache.
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
        let (width, height, format) = device_geometry();
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

    /// Blits the sub-rectangle `[x, x+w) × [y, y+h)` to the screen and refreshes it.
    ///
    /// Returns `true` on success. The rectangle must lie fully within the canvas.
    /// Note: on large-screen devices this is only cheap for *small* rectangles;
    /// for full-screen drawing use [`render_banded`] instead (see the type docs).
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

        self.draw_area(x, y, w, h)
            && unsafe {
                ecalls::display_refresh(
                    x as u32,
                    y as u32,
                    w as u32,
                    h as u32,
                    default_refresh_mode(self.format) as u32,
                ) == 0
            }
    }

    /// Draws `[x, x+w) × [y, y+h)` into the screen framebuffer without refreshing.
    /// `y` and `h` must already be multiples of 4. Returns `true` on success.
    fn draw_area(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        // Fast path: a full-width strip is already contiguous at the canvas stride,
        // so we can blit the backing buffer directly without copying.
        if x == 0 && w == self.width {
            let buf = &self.pixels[y * self.stride..(y + h) * self.stride];
            return unsafe {
                ecalls::display_blit(
                    0,
                    y as u32,
                    w as u32,
                    h as u32,
                    buf.as_ptr(),
                    buf.len(),
                    self.format as u32,
                )
            } == 0;
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
        unsafe {
            ecalls::display_blit(
                x as u32,
                y as u32,
                w as u32,
                h as u32,
                out.as_ptr(),
                out.len(),
                self.format as u32,
            ) == 0
        }
    }

    /// Draws this canvas's top `h` rows (full width) into the screen framebuffer at
    /// absolute screen row `screen_y`, without refreshing. Used by banded rendering.
    /// `screen_y` and `h` must be multiples of 4, `h <= height`. Returns `true` on success.
    fn blit_band_to_screen(&self, screen_y: usize, h: usize) -> bool {
        let buf = &self.pixels[0..h * self.stride];
        unsafe {
            ecalls::display_blit(
                0,
                screen_y as u32,
                self.width as u32,
                h as u32,
                buf.as_ptr(),
                buf.len(),
                self.format as u32,
            ) == 0
        }
    }

    /// Blits the entire canvas to the screen and refreshes.
    ///
    /// On large-screen devices, prefer [`render_banded`] — see the type docs.
    pub fn flush(&mut self) -> bool {
        self.flush_area(0, 0, self.width, self.height)
    }
}

/// Returns the current device's screen size `(width, height)` in pixels, without
/// allocating a framebuffer. Useful to lay out a scene before [`render_banded`].
pub fn device_screen_size() -> (usize, usize) {
    let (w, h, _) = device_geometry();
    (w, h)
}

/// Returns the current device's screen `(width, height, native pixel format)`.
fn device_geometry() -> (usize, usize, PixelFormat) {
    let size = ecalls::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE);
    let width = (size >> 16) as usize;
    let height = (size & 0xffff) as usize;
    let format = PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT))
        .expect("device reported an unknown pixel format");
    (width, height, format)
}

/// Default band height (in rows) for [`render_banded`]. A multiple of 4 chosen so a
/// band buffer stays resident in the VM's small data page cache.
pub const DEFAULT_BAND_ROWS: usize = 8;

/// Renders a full screen band by band, without ever allocating a full-screen
/// framebuffer (see the [`Canvas`] type docs for why that matters on large screens).
///
/// A single small band buffer of `band_rows` rows is allocated once and reused. For
/// each horizontal band starting at row `band_y0`, the buffer is cleared to
/// `background` (intensity `0..=15`), `draw(&mut band, band_y0)` is called, and the
/// band is blitted to screen rows `[band_y0, band_y0 + band_rows)`. A single panel
/// refresh is issued at the end. `draw` is therefore invoked `ceil(height/band_rows)`
/// times and must draw the same content each time (it should be a pure function of
/// the scene); each call draws into band-local coordinates given the band's
/// `band_y0` offset. `band_rows` is clamped to a multiple of 4 (>= 4).
///
/// Returns `true` if every band blit and the final refresh succeeded.
pub fn render_banded_raw<F>(
    width: usize,
    height: usize,
    format: PixelFormat,
    band_rows: usize,
    background: u8,
    mut draw: F,
) -> bool
where
    F: FnMut(&mut Canvas, usize),
{
    let band_rows = core::cmp::min((band_rows & !3).max(4), height);
    let mut band = Canvas::new(width, band_rows, format);

    let mut y0 = 0;
    let mut ok = true;
    while y0 < height {
        let bh = core::cmp::min(band_rows, height - y0);
        band.clear(background);
        draw(&mut band, y0);
        ok &= band.blit_band_to_screen(y0, bh);
        y0 += bh;
    }
    ok && unsafe {
        ecalls::display_refresh(
            0,
            0,
            width as u32,
            height as u32,
            default_refresh_mode(format) as u32,
        ) == 0
    }
}

/// Like [`render_banded_raw`], but using the current device's screen size and native
/// pixel format and [`DEFAULT_BAND_ROWS`].
pub fn render_banded_for_device_raw<F>(background: u8, draw: F) -> bool
where
    F: FnMut(&mut Canvas, usize),
{
    let (w, h, fmt) = device_geometry();
    render_banded_raw(w, h, fmt, DEFAULT_BAND_ROWS, background, draw)
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

/// The `DrawTarget` handed to a [`render_banded`] closure: a band `Canvas`
/// translated so the closure draws in absolute screen coordinates, with only the
/// current band's pixels kept. (`DrawTarget` is not object-safe, so this is the
/// concrete translated type rather than `dyn DrawTarget`.)
#[cfg(feature = "embedded-graphics")]
pub type BandTarget<'a> = embedded_graphics::draw_target::Translated<'a, Canvas>;

/// Renders a full screen band by band with `embedded-graphics`, without allocating a
/// full-screen framebuffer (see the [`Canvas`] type docs).
///
/// `draw` is called once per band with a [`BandTarget`] on which the V-App draws the
/// **whole scene in absolute screen coordinates**; the target is translated so that
/// only the pixels falling inside the current band are kept (the rest are clipped),
/// so app code is identical to drawing onto a full-screen `Canvas`. A single panel
/// refresh is issued at the end. `draw` must render the same content on every call.
///
/// Returns `true` if every band blit and the final refresh succeeded.
#[cfg(feature = "embedded-graphics")]
pub fn render_banded<F>(
    width: usize,
    height: usize,
    format: PixelFormat,
    band_rows: usize,
    background: u8,
    mut draw: F,
) -> bool
where
    F: FnMut(&mut BandTarget<'_>),
{
    use embedded_graphics::prelude::*;
    render_banded_raw(width, height, format, band_rows, background, |band, y0| {
        // Map absolute (x, y) -> band-local (x, y - y0). Pixels above the band go
        // negative (dropped by the DrawTarget's `< 0` guard) and pixels below exceed
        // the band height (dropped by `set_pixel`'s bounds check).
        let mut target = band.translated(Point::new(0, -(y0 as i32)));
        draw(&mut target);
    })
}

/// Like [`render_banded`], but using the current device's screen size and native
/// pixel format, with a caller-chosen `band_rows`.
#[cfg(feature = "embedded-graphics")]
pub fn render_banded_for_device_with<F>(band_rows: usize, background: u8, draw: F) -> bool
where
    F: FnMut(&mut BandTarget<'_>),
{
    let (w, h, fmt) = device_geometry();
    render_banded(w, h, fmt, band_rows, background, draw)
}

/// Like [`render_banded`], but using the current device's screen size and native
/// pixel format and [`DEFAULT_BAND_ROWS`].
#[cfg(feature = "embedded-graphics")]
pub fn render_banded_for_device<F>(background: u8, draw: F) -> bool
where
    F: FnMut(&mut BandTarget<'_>),
{
    render_banded_for_device_with(DEFAULT_BAND_ROWS, background, draw)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The banded loop must cover every row exactly once, in 4-row-aligned bands
    // (the last possibly short), in top-to-bottom order.
    #[test]
    fn test_render_banded_band_sequence() {
        let (w, h) = (40usize, 36usize); // 36 = 4*8 + 4 -> short last band with band_rows=8
        let mut bands: alloc::vec::Vec<(usize, usize)> = alloc::vec::Vec::new();
        let ok = render_banded_raw(w, h, PixelFormat::Gray4, 8, 0, |band, y0| {
            assert_eq!(band.width(), w);
            assert_eq!(band.height(), 8); // the reused buffer is always band_rows tall
            assert_eq!(y0 % 4, 0);
            // record (y0, remaining-or-band height); recompute bh from coverage below
            bands.push((y0, 0));
        });
        assert!(ok);
        let ys: alloc::vec::Vec<usize> = bands.iter().map(|(y, _)| *y).collect();
        assert_eq!(ys, alloc::vec![0, 8, 16, 24, 32]); // last band starts at 32, covers rows 32..36
    }

    // Rendering a scene band-by-band with the translation must reproduce exactly the
    // same image as rendering it into a full-size canvas. This validates the
    // translation sign, clipping at band boundaries, and the short last band.
    #[cfg(feature = "embedded-graphics")]
    #[test]
    fn test_banding_matches_full_render() {
        use embedded_graphics::{
            pixelcolor::Gray4,
            prelude::*,
            primitives::{Circle, Line, PrimitiveStyle, Rectangle},
        };

        // A scene drawn in absolute coordinates, generic over the draw target.
        fn scene<D>(t: &mut D)
        where
            D: DrawTarget<Color = Gray4>,
            D::Error: core::fmt::Debug,
        {
            let fg = Gray4::new(12);
            Rectangle::new(Point::new(2, 3), Size::new(30, 28))
                .into_styled(PrimitiveStyle::with_stroke(fg, 2))
                .draw(t)
                .unwrap();
            Line::new(Point::new(0, 0), Point::new(39, 35))
                .into_styled(PrimitiveStyle::with_stroke(Gray4::new(15), 1))
                .draw(t)
                .unwrap();
            Circle::new(Point::new(10, 10), 16)
                .into_styled(PrimitiveStyle::with_fill(Gray4::new(7)))
                .draw(t)
                .unwrap();
        }

        let (w, h) = (40usize, 36usize); // multiple of 4, not of 8 -> exercises short band
        let band_rows = 8usize;
        let bg = 2u8;

        // Reference: full-size canvas.
        let mut full = Canvas::new(w, h, PixelFormat::Gray4);
        full.clear(bg);
        scene(&mut full);

        // Reconstruct via the same banded translation render_banded uses, copying each
        // band into a full-size buffer instead of blitting to the screen.
        let mut recon = Canvas::new(w, h, PixelFormat::Gray4);
        recon.clear(bg);
        let mut y0 = 0;
        while y0 < h {
            let bh = core::cmp::min(band_rows, h - y0);
            let mut band = Canvas::new(w, band_rows, PixelFormat::Gray4);
            band.clear(bg);
            scene(&mut band.translated(Point::new(0, -(y0 as i32))));
            for ry in 0..bh {
                for rx in 0..w {
                    recon.set_pixel(rx, y0 + ry, band.pixel(rx, ry));
                }
            }
            y0 += bh;
        }

        assert_eq!(full.pixels, recon.pixels, "banded render must match full render");
    }
}
