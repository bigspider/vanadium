//! [`Capabilities`]: what a device's screen can do and how it prefers to be refreshed.
//!
//! UI code should branch on these values rather than on `target_os`, so adding a new
//! device is a matter of what its capabilities report — not scattered `cfg`s. The
//! values come from the `DEVICE_PROPERTY_*` queries (in particular the `FEATURE_*`
//! bits), never from a device-id table.

use common::ecall_constants::{
    DisplayGranularity, Font, PixelFormat, DEVICE_PROPERTY_DISPLAY_GRANULARITY,
    DEVICE_PROPERTY_FEATURES, DEVICE_PROPERTY_MAX_TEXT_LEN, DEVICE_PROPERTY_PIXEL_FORMAT,
    DISPLAY_MAX_TEXT_LEN, FEATURE_ACCEL_TEXT, FEATURE_FAST_MONO_REFRESH,
    FEATURE_PARTIAL_REFRESH, FEATURE_TOUCH,
};

use crate::ecalls;
use crate::ux::canvas::device_screen_size;

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
    /// Whether the device has a hardware font engine (`FEATURE_ACCEL_TEXT`). True on
    /// every current device; kept explicit so a future device without it falls back
    /// cleanly.
    pub native_text: bool,
    pub input: InputModel,
    /// Whether the panel can refresh a sub-rectangle (vs. only the whole screen).
    pub partial_refresh: bool,
    /// Whether the panel has a fast black-&-white refresh mode (much quicker than the
    /// full-quality refresh on the reflective Stax/Flex panels).
    pub fast_mono_refresh: bool,
    /// The display's alignment constraints for `display_blit` rectangles. UI code must
    /// align flushes with these values, never a hardcoded constant.
    pub granularity: DisplayGranularity,
    /// Maximum byte length accepted per `display_draw_text` / `display_text_width` call.
    pub max_text_len: usize,
    /// Metrics for the three semantic fonts, indexed by [`Font::index`].
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

    let pixel_format =
        PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT))
            .unwrap_or(PixelFormat::Gray4);

    let features = ecalls::get_device_property(DEVICE_PROPERTY_FEATURES);
    let input = if features & FEATURE_TOUCH != 0 {
        InputModel::Pointer
    } else {
        InputModel::TwoButton
    };

    // An unsupported property returns 0, which DisplayGranularity::from_u32 rejects;
    // fall back to the most constrained granularity any current device has, so a
    // too-old VM gets conservative (correct) alignment rather than broken blits.
    let granularity =
        DisplayGranularity::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_DISPLAY_GRANULARITY))
            .unwrap_or(DisplayGranularity {
                x: 1,
                y: 4,
                w: 1,
                h: 4,
            });

    let max_text_len = match ecalls::get_device_property(DEVICE_PROPERTY_MAX_TEXT_LEN) {
        0 => DISPLAY_MAX_TEXT_LEN, // property not supported: assume this tree's limit
        n => n as usize,
    };

    Capabilities {
        size: Size::new(w as u32, h as u32),
        pixel_format,
        native_text: features & FEATURE_ACCEL_TEXT != 0,
        input,
        partial_refresh: features & FEATURE_PARTIAL_REFRESH != 0,
        fast_mono_refresh: features & FEATURE_FAST_MONO_REFRESH != 0,
        granularity,
        max_text_len,
        fonts: [
            font_metrics(Font::Regular),
            font_metrics(Font::Bold),
            font_metrics(Font::Large),
        ],
    }
}
