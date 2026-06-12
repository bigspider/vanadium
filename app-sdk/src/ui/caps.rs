//! [`Capabilities`]: what a device's screen can do and how it prefers to be refreshed.
//!
//! UI code should branch on these values rather than on `target_os`, so adding a new
//! device is a matter of what its capabilities report — not scattered `cfg`s.

use common::ecall_constants::{Font, PixelFormat, DEVICE_PROPERTY_PIXEL_FORMAT};

use crate::ecalls;
use crate::ux::canvas::device_screen_size;
use crate::ux::has_page_api;

use super::Size;

/// How the user drives the UI.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum InputModel {
    /// A touch screen delivering absolute `(x, y)` positions.
    Pointer,
    /// The two-button Nano devices (left / right / both).
    TwoButton,
}

/// Vertical metrics of an OS font, in pixels, for laying out text rows.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FontMetrics {
    pub height: u16,
    pub line_height: u16,
}

/// A description of the current device's screen and input, queried once at startup.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub size: Size,
    pub pixel_format: PixelFormat,
    /// Whether the device has a hardware font engine (`nbgl_drawText`). Always true on the
    /// supported devices; kept explicit so a future device without it falls back cleanly.
    pub native_text: bool,
    pub input: InputModel,
    /// Whether the panel can refresh a sub-rectangle (vs. only the whole screen).
    pub partial_refresh: bool,
    /// Whether the panel has a fast black-&-white refresh mode (much quicker than the
    /// full-color refresh on the reflective Stax/Flex panels).
    pub fast_mono_refresh: bool,
    /// Metrics for the three semantic fonts, indexed by [`Font`] as `usize`.
    pub fonts: [FontMetrics; 3],
}

impl Capabilities {
    /// Metrics for a given font.
    pub fn font(&self, font: Font) -> FontMetrics {
        self.fonts[font.index()]
    }
}

fn font_metrics(font: Font) -> FontMetrics {
    // The fonts queried here are the SDK's own roles, so an error (negative) can only
    // mean a VM older than this SDK; degrade to zero metrics rather than panic.
    let packed = unsafe { ecalls::display_font_metrics(font as u32) }.max(0) as u32;
    FontMetrics {
        height: (packed >> 16) as u16,
        line_height: (packed & 0xffff) as u16,
    }
}

/// Queries the current device's [`Capabilities`].
pub fn capabilities() -> Capabilities {
    let (w, h) = device_screen_size();

    let pixel_format = PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT))
        .unwrap_or(PixelFormat::Gray4);

    let input = if has_page_api() {
        InputModel::Pointer
    } else {
        InputModel::TwoButton
    };

    Capabilities {
        size: Size::new(w as u32, h as u32),
        pixel_format,
        native_text: true,
        input,
        // NBGL's refresh syscall takes an area and supports a fast B&W mode on every
        // supported panel; the policy decides whether to use them.
        partial_refresh: true,
        fast_mono_refresh: true,
        fonts: [
            font_metrics(Font::Regular),
            font_metrics(Font::Bold),
            font_metrics(Font::Large),
        ],
    }
}
