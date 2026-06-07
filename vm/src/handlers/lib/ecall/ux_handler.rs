#[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
use core::ffi::c_void;
use core::mem::MaybeUninit;

use alloc::{ffi::CString, string::String, vec::Vec};

use ledger_device_sdk::sys;

use common::ux::{Page, Step};

use super::bitmaps::ToIconDetails;

use super::CommEcallError;

#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_CONFIRM: u8 = 1;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_CONFIRM_REJECT: u8 = 2;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_QUIT: u8 = 3;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_SKIP: u8 = 4;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_NAVIGATION: u8 = 5;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_TITLE: u8 = 6;
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
const TOKEN_TOPRIGHT: u8 = 7;

// The device's native NBGL color depth, used for the area `bpp` of the accelerated
// draw ops and refreshes. Stax/Flex are 4bpp grayscale; Apex and the Nanos are 1bpp.
#[cfg(any(target_os = "stax", target_os = "flex"))]
const NATIVE_BPP: sys::nbgl_bpp_t = sys::NBGL_BPP_4;
#[cfg(any(target_os = "apex_p", target_os = "nanosplus", target_os = "nanox"))]
const NATIVE_BPP: sys::nbgl_bpp_t = sys::NBGL_BPP_1;

// Maps a semantic [`common::ecall_constants::Font`] to the device's matching NBGL font
// id. The font sets differ per device (see `nbgl_fonts.h`); these are the regular /
// semibold / large roles for the current target.
fn font_id(font: common::ecall_constants::Font) -> sys::nbgl_font_id_e {
    use common::ecall_constants::Font;
    // BAGL_FONT_INTER_{REGULAR,SEMIBOLD,MEDIUM} for the device's font height, or the
    // OPEN_SANS / NANO* equivalents on the small screens (see nbgl_fonts.h enum values).
    #[cfg(target_os = "stax")] // SMALL_FONT_HEIGHT == 24
    let id: u8 = match font {
        Font::Regular => 0,
        Font::Bold => 1,
        Font::Large => 2,
    };
    #[cfg(target_os = "flex")] // SMALL_FONT_HEIGHT == 28
    let id: u8 = match font {
        Font::Regular => 11,
        Font::Bold => 12,
        Font::Large => 13,
    };
    #[cfg(target_os = "apex_p")] // SMALL_FONT_HEIGHT == 18
    let id: u8 = match font {
        Font::Regular => 17,
        Font::Bold => 18,
        Font::Large => 19,
    };
    #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
    let id: u8 = match font {
        Font::Regular => 10, // OPEN_SANS_REGULAR_11px_1bpp
        Font::Bold => 8,     // OPEN_SANS_EXTRABOLD_11px_1bpp
        Font::Large => 9,    // OPEN_SANS_LIGHT_16px_1bpp
    };
    id as sys::nbgl_font_id_e
}

// We use MaybeUninit to make sure that the static variable does not create
// a .data section, which is not allowed.
static mut LAST_EVENT: MaybeUninit<Option<(common::ux::EventCode, common::ux::EventData)>> =
    MaybeUninit::uninit();
static mut LAST_EVENT_INITIALIZED: bool = false;

fn init_last_event() {
    #[allow(static_mut_refs)]
    unsafe {
        if !LAST_EVENT_INITIALIZED {
            LAST_EVENT.write(None);
            LAST_EVENT_INITIALIZED = true;
        }
    }
}

pub fn get_last_event() -> Option<(common::ux::EventCode, common::ux::EventData)> {
    init_last_event();
    // Safe in a single-threaded environment
    #[allow(static_mut_refs)]
    unsafe {
        LAST_EVENT.assume_init_mut().take()
    }
}

/// Builds a [`Touch`](common::ux::EventCode::Touch) event from a decoded seph finger
/// packet and stores it for delivery to the guest's `get_event`.
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
pub fn store_touch_event(x: u16, y: u16, state: u8) {
    use common::ux::{EventCode, EventData, TouchEvent, TouchState};
    // seph finger state: 0x01 (SEPROXYHAL_TAG_FINGER_EVENT_TOUCH) = pressed; 0x02 = released.
    let state = if state == 1 {
        TouchState::Pressed
    } else {
        TouchState::Released
    };
    let mut event_data = EventData::default();
    event_data.touch = TouchEvent::new(x, y, state);
    store_new_event(EventCode::Touch, event_data);
}

/// Builds a [`Button`](common::ux::EventCode::Button) event from a decoded button event
/// and stores it for delivery to the guest's `get_event`.
#[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
pub fn store_button_event(btn: ledger_device_sdk::buttons::ButtonEvent) {
    use common::ux::{ButtonEvent as CButton, EventCode, EventData};
    use ledger_device_sdk::buttons::ButtonEvent;
    let mapped = match btn {
        ButtonEvent::LeftButtonPress => CButton::LeftPress,
        ButtonEvent::RightButtonPress => CButton::RightPress,
        ButtonEvent::BothButtonsPress => CButton::BothPress,
        ButtonEvent::LeftButtonRelease => CButton::LeftRelease,
        ButtonEvent::RightButtonRelease => CButton::RightRelease,
        ButtonEvent::BothButtonsRelease => CButton::BothRelease,
    };
    let mut event_data = EventData::default();
    event_data.button = mapped;
    store_new_event(EventCode::Button, event_data);
}

fn store_new_event(event_code: common::ux::EventCode, event_data: common::ux::EventData) {
    init_last_event();
    // We store the new event if there is no buffered event or only a ticker. Additionally,
    // we coalesce consecutive touch events: a newer touch overwrites a still-unconsumed one
    // so the latest finger state — in particular a release ending a drag — is never dropped
    // behind a stale press while the guest is busy (e.g. mid-redraw). Without this, a lost
    // release leaves a custom GUI thinking the finger is still down. Other event kinds
    // (notably NBGL `Action`s) are still preserved, so page UX is unaffected.
    #[allow(static_mut_refs)]
    unsafe {
        if !LAST_EVENT_INITIALIZED {
            LAST_EVENT.write(None);
            LAST_EVENT_INITIALIZED = true;
        }
        let last_event = LAST_EVENT.assume_init_mut();
        let replace = match last_event.as_ref() {
            None => true,
            Some((code, _)) => {
                *code == common::ux::EventCode::Ticker
                    || (*code == common::ux::EventCode::Touch
                        && event_code == common::ux::EventCode::Touch)
            }
        };
        if replace {
            *last_event = Some((event_code, event_data));
        }
    }
}

// nbgl_layoutTouchCallback_t
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
unsafe extern "C" fn layout_touch_callback(token: core::ffi::c_int, index: u8) {
    let action = match (token as u8, index) {
        (TOKEN_CONFIRM, _) => common::ux::Action::Confirm,
        (TOKEN_CONFIRM_REJECT, 0) => common::ux::Action::Confirm,
        (TOKEN_CONFIRM_REJECT, 1) => common::ux::Action::Reject,
        (TOKEN_QUIT, _) => common::ux::Action::Quit,
        (TOKEN_SKIP, _) => common::ux::Action::Skip,
        (TOKEN_NAVIGATION, idx) => {
            let cur_page = get_ux_handler().cur_page;

            let diff = idx as isize - cur_page as isize;
            match diff {
                -1 => common::ux::Action::PreviousPage,
                1 => common::ux::Action::NextPage,
                _ => {
                    crate::println!("Unexpected index, cur_page is {}", cur_page);
                    return;
                }
            }
        }
        (TOKEN_TITLE, _) => common::ux::Action::TitleBack,
        (TOKEN_TOPRIGHT, _) => common::ux::Action::TopRight,
        _ => {
            crate::println!("Event unhandled");
            return;
        }
    };

    let mut event_data = common::ux::EventData::default();
    event_data.action = action;
    store_new_event(common::ux::EventCode::Action, event_data);
}

// nbgl_stepButtonCallback_t
#[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
unsafe extern "C" fn step_button_callback(
    _layout: *mut c_void,
    button_event: sys::nbgl_buttonEvent_t,
) {
    let action = match button_event {
        // see nbgl_buttonEvent_t
        0 => common::ux::Action::PreviousPage,
        1 => common::ux::Action::NextPage,
        4 => common::ux::Action::Confirm,
        _ => {
            crate::println!("Unhandled button event: {:?}", button_event);
            return;
        }
    };

    let mut event_data = common::ux::EventData::default();
    event_data.action = action;
    store_new_event(common::ux::EventCode::Action, event_data);
}

// encapsulates all the global state related to Events and UX handling

pub struct UxHandler {
    cstrings: Vec<CString>,
    #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
    step_handle: *mut c_void, // handle returned by nbgl when drawing a step; should be freed before drawing a new step
    #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
    cur_page: u8,
    #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
    page_handle: *mut sys::nbgl_page_t, // handle returned by nbgl when drawing a page; should be freed before drawing a new page
    #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
    tag_value_list: Vec<sys::nbgl_contentTagValue_t>,
}

// Global static variable to hold the singleton instance
static mut UX_HANDLER: core::mem::MaybeUninit<UxHandler> = core::mem::MaybeUninit::uninit();
static mut UX_HANDLER_INITIALIZED: bool = false;

pub fn init_ux_handler() -> &'static mut UxHandler {
    unsafe {
        if UX_HANDLER_INITIALIZED {
            panic!("UxHandler already initialized");
        }

        #[allow(static_mut_refs)] // it's safe as we are in single-threaded mode
        UX_HANDLER.write(UxHandler::new());
        UX_HANDLER_INITIALIZED = true;

        #[allow(static_mut_refs)] // it's safe as we are in single-threaded mode
        UX_HANDLER.assume_init_mut()
    }
}

#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
pub fn get_ux_handler() -> &'static mut UxHandler {
    unsafe {
        if !UX_HANDLER_INITIALIZED {
            panic!("UxHandler not initialized");
        }

        #[allow(static_mut_refs)] // it's safe as we are in single-threaded mode
        UX_HANDLER.assume_init_mut()
    }
}

pub fn drop_ux_handler() {
    unsafe {
        if !UX_HANDLER_INITIALIZED {
            return;
        }

        #[allow(static_mut_refs)] // it's safe as we are in single-threaded mode
        let handler = UX_HANDLER.assume_init_mut();
        handler.release_handle();
        handler.clear_cstrings();

        UX_HANDLER_INITIALIZED = false;
    }
}

impl UxHandler {
    // We keep the constructor private in order to manage the singleton instance
    fn new() -> Self {
        #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
        return Self {
            cstrings: Vec::new(),
            step_handle: core::ptr::null_mut(),
        };
        #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
        return Self {
            cstrings: Vec::new(),
            cur_page: 0,
            page_handle: core::ptr::null_mut(),
            tag_value_list: Vec::new(),
        };
    }

    pub fn clear_cstrings(&mut self) {
        self.cstrings.clear();
        #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
        self.tag_value_list.clear();
    }

    #[inline(always)]
    pub unsafe fn alloc_cstring(
        &mut self,
        string: Option<&String>,
    ) -> Result<*const core::ffi::c_char, CommEcallError> {
        if let Some(string) = string {
            self.cstrings.push(CString::new(string.clone())?);
            return Ok(self.cstrings[self.cstrings.len() - 1].as_ptr());
        }
        Ok(core::ptr::null())
    }

    // This should always be called before drawing a new step or page, in order to
    // make sure that the resources of the previous step/page are released
    fn release_handle(&mut self) {
        #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
        unsafe {
            if !self.page_handle.is_null() {
                sys::nbgl_pageRelease(self.page_handle);
                self.page_handle = core::ptr::null_mut();
            }
        }

        #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
        unsafe {
            if !self.step_handle.is_null() {
                sys::nbgl_stepRelease(self.step_handle);
                self.step_handle = core::ptr::null_mut();
            }
        }
    }

    #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
    pub fn show_page(&mut self, _page: &Page) -> Result<(), CommEcallError> {
        Err(CommEcallError::UnhandledEcall)
    }

    /// Draws a horizontal band of pixels onto the screen framebuffer.
    ///
    /// `pixels` is the band in the SDK's row-major, left-to-right [`PixelFormat`]
    /// packing (`h` rows of `stride(w)` bytes). NBGL's `nbgl_frontDrawImage` instead
    /// consumes its buffer **column-major, right-to-left, top-to-bottom**, packed
    /// MSB-first (high nibble first for 4BPP) with no per-column padding (see
    /// `nbgl_driver_drawImage`), so we transpose into that layout here. `y` and `h`
    /// must be multiples of 4 (an NBGL constraint, enforced by the caller).
    ///
    /// The blit ECALL splits a rectangle into bands to bound the VM's scratch
    /// memory; call [`UxHandler::blit_refresh`] once afterwards to push the drawn
    /// region to the panel.
    ///
    /// The low-level `nbgl_frontDrawImage` / `nbgl_frontRefreshArea` entry points are
    /// BOLOS syscalls whose C stubs are linked into `ledger_secure_sdk_sys` but are
    /// not exposed by its generated bindings, so we declare them here ourselves using
    /// the bound NBGL types and constants. The same syscalls drive every screen
    /// model; only the bit depth and color handling differ per `PixelFormat`.
    pub fn blit_band(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        format: common::ecall_constants::PixelFormat,
        pixels: &[u8],
        out: &mut [u8],
    ) -> Result<(), CommEcallError> {
        use common::ecall_constants::PixelFormat;

        extern "C" {
            fn nbgl_frontDrawImage(
                area: *const sys::nbgl_area_t,
                buffer: *const u8,
                transformation: sys::nbgl_transformation_t,
                color_map: sys::nbgl_color_map_t,
            );
        }

        // For BPP_4 the color_map is INVALID_COLOR_MAP (no remapping, the grayscale
        // values are used directly). For BPP_1, NBGL interprets the color_map as the
        // *foreground* color (drawn where a bit is set), while bit-clear pixels take
        // the area's backgroundColor (see nbgl_types.h). We pack Mono1 with the bit
        // set for "on"/light pixels, so foreground = WHITE, background = BLACK.
        const INVALID_COLOR_MAP: sys::nbgl_color_map_t = 0;
        let (bpp, background, color_map) = match format {
            PixelFormat::Gray4 => (sys::NBGL_BPP_4, sys::WHITE, INVALID_COLOR_MAP),
            PixelFormat::Mono1 => (sys::NBGL_BPP_1, sys::BLACK, sys::WHITE as sys::nbgl_color_map_t),
        };

        let w = w as usize;
        let h = h as usize;
        let in_stride = format.stride(w);

        // Transpose row-major (our layout) into NBGL's column-major, right-to-left
        // layout, into the caller-provided scratch buffer. Pixels are emitted from
        // the rightmost column, top to bottom, with no padding between columns; this
        // also yields the correct (un-mirrored) orientation, so no transformation is
        // needed.
        let out_len = (w * h * format.bits_per_pixel() + 7) / 8;
        let out = &mut out[..out_len];
        out.fill(0);
        let mut k = 0usize; // index of the pixel being emitted
        for ox in (0..w).rev() {
            for oy in 0..h {
                match format {
                    PixelFormat::Gray4 => {
                        let byte = pixels[oy * in_stride + ox / 2];
                        let v = if ox % 2 == 0 { byte >> 4 } else { byte & 0x0f };
                        if k % 2 == 0 {
                            out[k / 2] |= v << 4;
                        } else {
                            out[k / 2] |= v;
                        }
                    }
                    PixelFormat::Mono1 => {
                        let bit = (pixels[oy * in_stride + ox / 8] >> (7 - (ox % 8))) & 1;
                        if bit == 1 {
                            out[k / 8] |= 1 << (7 - (k % 8));
                        }
                    }
                }
                k += 1;
            }
        }

        let area = sys::nbgl_area_t {
            x0: x as i16,
            y0: y as i16,
            width: w as u16,
            height: h as u16,
            backgroundColor: background,
            bpp,
        };

        unsafe {
            nbgl_frontDrawImage(
                &area,
                out.as_ptr(),
                sys::NO_TRANSFORMATION as sys::nbgl_transformation_t,
                color_map,
            );
        }

        Ok(())
    }

    /// Pushes a previously drawn rectangle to the physical panel, using the requested
    /// [`RefreshMode`](common::ecall_constants::RefreshMode).
    pub fn blit_refresh(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        mode: common::ecall_constants::RefreshMode,
    ) -> Result<(), CommEcallError> {
        use common::ecall_constants::RefreshMode;

        extern "C" {
            fn nbgl_frontRefreshArea(
                area: *const sys::nbgl_area_t,
                mode: sys::nbgl_refresh_mode_t,
                post_refresh: sys::nbgl_post_refresh_t,
            );
        }

        let nbgl_mode = match mode {
            RefreshMode::FullColor => sys::FULL_COLOR_REFRESH,
            RefreshMode::Partial => sys::FULL_COLOR_PARTIAL_REFRESH,
            RefreshMode::BlackWhite => sys::BLACK_AND_WHITE_REFRESH,
            RefreshMode::BlackWhiteFast => sys::BLACK_AND_WHITE_FAST_REFRESH,
        };

        let area = sys::nbgl_area_t {
            x0: x as i16,
            y0: y as i16,
            width: w as u16,
            height: h as u16,
            backgroundColor: sys::WHITE,
            bpp: NATIVE_BPP,
        };

        unsafe {
            nbgl_frontRefreshArea(&area, nbgl_mode, sys::POST_REFRESH_FORCE_POWER_ON);
        }

        Ok(())
    }

    /// Fills a rectangle with a solid palette color, directly in the OS framebuffer
    /// (no guest framebuffer, no per-pixel transfer). Does not refresh the panel.
    ///
    /// `nbgl_frontDrawRect` aligns `y0`/`height` to the hardware vertical alignment
    /// itself (preserving the partial top/bottom rows), so unlike `blit_band` the
    /// rectangle does not have to be 4-row aligned.
    pub fn fill_rect(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color: common::ecall_constants::Color,
    ) -> Result<(), CommEcallError> {
        extern "C" {
            fn nbgl_frontDrawRect(area: *const sys::nbgl_area_t);
        }

        let area = sys::nbgl_area_t {
            x0: x as i16,
            y0: y as i16,
            width: w as u16,
            height: h as u16,
            backgroundColor: color as u8 as sys::color_t,
            bpp: NATIVE_BPP,
        };

        unsafe {
            nbgl_frontDrawRect(&area);
        }

        Ok(())
    }

    /// Draws a UTF-8 string with an OS font directly in the framebuffer. Does not
    /// refresh the panel. The text background is assumed light (`WHITE`) for font
    /// anti-aliasing; draw text over light fills for best results.
    pub fn draw_text(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        text: &[u8],
        font: common::ecall_constants::Font,
        color: common::ecall_constants::Color,
    ) -> Result<(), CommEcallError> {
        extern "C" {
            fn nbgl_drawText(
                area: *const sys::nbgl_area_t,
                text: *const core::ffi::c_char,
                text_len: u16,
                font_id: sys::nbgl_font_id_e,
                font_color: sys::color_t,
            ) -> sys::nbgl_font_id_e;
        }

        let area = sys::nbgl_area_t {
            x0: x as i16,
            y0: y as i16,
            width: w as u16,
            height: h as u16,
            backgroundColor: sys::WHITE,
            bpp: NATIVE_BPP,
        };

        unsafe {
            nbgl_drawText(
                &area,
                text.as_ptr() as *const core::ffi::c_char,
                text.len() as u16,
                font_id(font),
                color as u8 as sys::color_t,
            );
        }

        Ok(())
    }

    /// Returns the rendered width in pixels of a UTF-8 string in an OS font, without
    /// drawing it — so a guest UI can lay out text without rasterizing it. Backed by the
    /// `nbgl_getTextWidth` syscall, which expects a NUL-terminated string.
    pub fn text_width(
        &mut self,
        text: &[u8],
        font: common::ecall_constants::Font,
    ) -> Result<u16, CommEcallError> {
        extern "C" {
            fn nbgl_getTextWidth(
                font_id: sys::nbgl_font_id_e,
                text: *const core::ffi::c_char,
            ) -> u16;
        }
        // nbgl_getTextWidth needs a C string; reject interior NULs (width 0).
        let Ok(cstr) = CString::new(text) else {
            return Ok(0);
        };
        let w = unsafe { nbgl_getTextWidth(font_id(font), cstr.as_ptr()) };
        Ok(w)
    }

    /// Returns `(height, line_height)` in pixels for an OS font, for row layout. Backed by
    /// the `nbgl_getFontHeight` / `nbgl_getFontLineHeight` syscalls.
    pub fn font_metrics(&self, font: common::ecall_constants::Font) -> (u8, u8) {
        extern "C" {
            fn nbgl_getFontHeight(font_id: sys::nbgl_font_id_e) -> u8;
            fn nbgl_getFontLineHeight(font_id: sys::nbgl_font_id_e) -> u8;
        }
        let id = font_id(font);
        unsafe { (nbgl_getFontHeight(id), nbgl_getFontLineHeight(id)) }
    }

    #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
    pub fn show_page(&mut self, page: &Page) -> Result<(), CommEcallError> {
        match page {
            common::ux::Page::Spinner { text } => unsafe {
                self.release_handle();
                self.page_handle = sys::nbgl_pageDrawSpinner(self.alloc_cstring(Some(text))?, 0);
            },
            common::ux::Page::Info { icon, text } => unsafe {
                self.release_handle();
                self.clear_cstrings();

                let ticker_config = sys::nbgl_screenTickerConfiguration_t {
                    tickerCallback: None, // we could put a callback here if we had a timer
                    tickerValue: 0,       // no timer
                    tickerIntervale: 0,   // not periodic
                };

                let page_info = sys::nbgl_pageInfoDescription_t {
                    centeredInfo: sys::nbgl_contentCenteredInfo_t {
                        text1: self.alloc_cstring(Some(text))?,
                        text2: core::ptr::null(),
                        text3: core::ptr::null(),
                        icon: icon.to_icon_details(),
                        onTop: false,
                        style: sys::LARGE_CASE_INFO,
                        offsetY: 0,
                    },
                    topRightStyle: sys::NO_BUTTON_STYLE,
                    bottomButtonStyle: sys::NO_BUTTON_STYLE,
                    topRightToken: 0,
                    bottomButtonsToken: 0,
                    footerText: core::ptr::null(),
                    footerToken: 1,
                    tapActionText: core::ptr::null(),
                    isSwipeable: true,
                    tapActionToken: 2,
                    actionButtonText: core::ptr::null(),
                    actionButtonIcon: core::ptr::null(),
                    actionButtonStyle: sys::BLACK_BACKGROUND,
                    tuneId: sys::TUNE_TAP_CASUAL,
                };

                self.page_handle = sys::nbgl_pageDrawInfo(
                    None,
                    &ticker_config, // or core::ptr::null()
                    &page_info,
                );
            },
            common::ux::Page::ConfirmReject {
                title,
                text,
                confirm,
                reject,
            } => unsafe {
                self.release_handle();
                self.clear_cstrings();

                let page_confirmation_description = sys::nbgl_pageConfirmationDescription_s {
                    centeredInfo: sys::nbgl_contentCenteredInfo_t {
                        text1: self.alloc_cstring(Some(title))?,
                        text2: self.alloc_cstring(Some(text))?,
                        text3: core::ptr::null(),
                        icon: core::ptr::null(),
                        onTop: false,
                        style: sys::LARGE_CASE_INFO,
                        offsetY: 0,
                    },
                    confirmationText: self.alloc_cstring(Some(confirm))?,
                    confirmationToken: TOKEN_CONFIRM_REJECT,
                    cancelText: self.alloc_cstring(Some(reject))?,
                    cancelToken: 255, // appears to be ignored
                    tuneId: sys::TUNE_TAP_CASUAL,
                    modal: false,
                };
                self.page_handle = sys::nbgl_pageDrawConfirmation(
                    Some(layout_touch_callback),
                    &page_confirmation_description,
                );
            },
            common::ux::Page::GenericPage {
                navigation_info,
                page_content_info,
            } => unsafe {
                self.release_handle();
                self.clear_cstrings();

                let nav_info = navigation_info
                    .as_ref()
                    .map(|ni| {
                        get_ux_handler().cur_page = ni.active_page as u8;

                        let common::ux::NavInfo::NavWithButtons {
                            has_back_button,
                            has_page_indicator,
                            quit_text,
                        } = &ni.nav_info;

                        if ni.n_pages > 255 || ni.active_page >= ni.n_pages {
                            return Err(CommEcallError::InvalidParameters(
                                "Invalid navigation info",
                            ));
                        }

                        Ok(sys::nbgl_pageNavigationInfo_t {
                            activePage: ni.active_page as u8,
                            nbPages: ni.n_pages as u8,
                            quitToken: TOKEN_QUIT,
                            navType: sys::NAV_WITH_BUTTONS,
                            progressIndicator: true,
                            tuneId: 0,
                            skipText: self.alloc_cstring(ni.skip_text.as_ref())?,
                            skipToken: TOKEN_SKIP,
                            __bindgen_anon_1:
                                sys::nbgl_pageMultiScreensDescription_s__bindgen_ty_1 {
                                    navWithButtons: sys::nbgl_pageNavWithButtons_s {
                                        quitButton: quit_text.is_some(),
                                        backButton: *has_back_button,
                                        visiblePageIndicator: *has_page_indicator, // only has any effect on Flex
                                        navToken: TOKEN_NAVIGATION,
                                        quitText: self.alloc_cstring(quit_text.as_ref())?,
                                    },
                                },
                        })
                    })
                    .transpose()?;
                let navigation_info = match nav_info {
                    Some(ref ni) => ni as *const _,
                    None => core::ptr::null(),
                };

                match &page_content_info.page_content {
                    common::ux::PageContent::TextSubtext { text, subtext } => {
                        self.page_handle = sys::nbgl_pageDrawGenericContent(
                            Some(layout_touch_callback),
                            navigation_info,
                            &mut sys::nbgl_pageContent_t {
                                title: self.alloc_cstring(page_content_info.title.as_ref())?,
                                isTouchableTitle: false, // unused in nbgl
                                titleToken: TOKEN_TITLE,
                                tuneId: 0,
                                topRightToken: 255,              // not implemented
                                topRightIcon: core::ptr::null(), // not implemented
                                type_: sys::CENTERED_INFO,
                                __bindgen_anon_1: sys::nbgl_pageContent_s__bindgen_ty_1 {
                                    centeredInfo: sys::nbgl_contentCenteredInfo_t {
                                        text1: self.alloc_cstring(Some(text))?,
                                        text2: self.alloc_cstring(Some(subtext))?,
                                        text3: core::ptr::null(),
                                        icon: core::ptr::null(),
                                        onTop: false,
                                        style: sys::LARGE_CASE_INFO,
                                        offsetY: 0,
                                    },
                                },
                            },
                        );
                    }
                    common::ux::PageContent::TagValueList { list } => {
                        self.tag_value_list = list
                            .iter()
                            .map(|t| {
                                let mut res = sys::nbgl_contentTagValue_t::default();
                                res.item = self.alloc_cstring(Some(&t.tag))?;
                                res.value = self.alloc_cstring(Some(&t.value))?;
                                Ok(res)
                            })
                            .collect::<Result<Vec<_>, CommEcallError>>()?;

                        let pairs_ptr = self.tag_value_list.as_ptr();
                        let nb_pairs = self.tag_value_list.len() as u8;
                        let title_ptr = self.alloc_cstring(page_content_info.title.as_ref())?;

                        self.page_handle = sys::nbgl_pageDrawGenericContent(
                            Some(layout_touch_callback),
                            navigation_info,
                            &mut sys::nbgl_pageContent_t {
                                title: title_ptr,
                                isTouchableTitle: false, // unused in nbgl
                                titleToken: TOKEN_TITLE,
                                tuneId: 0,
                                topRightToken: 255,              // not implemented
                                topRightIcon: core::ptr::null(), // not implemented
                                type_: sys::TAG_VALUE_LIST,
                                __bindgen_anon_1: sys::nbgl_pageContent_s__bindgen_ty_1 {
                                    tagValueList: sys::nbgl_contentTagValueList_t {
                                        pairs: pairs_ptr,
                                        callback: None,
                                        nbPairs: nb_pairs,
                                        startIndex: 0, // unused if no callback
                                        nbMaxLinesForValue: 0,
                                        token: 255,
                                        smallCaseForValue: false,
                                        wrapping: true,
                                        actionCallback: None, // not implemented, no events from the tagvalues
                                        ..Default::default()
                                    },
                                },
                            },
                        );
                    }
                    common::ux::PageContent::ConfirmationButton { text, button_text } => {
                        self.page_handle = sys::nbgl_pageDrawGenericContent(
                            Some(layout_touch_callback),
                            navigation_info,
                            &mut sys::nbgl_pageContent_t {
                                title: self.alloc_cstring(page_content_info.title.as_ref())?,
                                isTouchableTitle: false, // unused in nbgl
                                titleToken: TOKEN_TITLE,
                                tuneId: 0,
                                topRightToken: 255,              // not implemented
                                topRightIcon: core::ptr::null(), // not implemented
                                type_: sys::INFO_BUTTON,
                                __bindgen_anon_1: sys::nbgl_pageContent_s__bindgen_ty_1 {
                                    infoButton: sys::nbgl_contentInfoButton_t {
                                        text: self.alloc_cstring(Some(text))?,
                                        icon: core::ptr::null(),
                                        buttonText: self.alloc_cstring(Some(button_text))?,
                                        buttonToken: TOKEN_CONFIRM,
                                        tuneId: 0,
                                    },
                                },
                            },
                        );
                    }
                    common::ux::PageContent::ConfirmationLongPress {
                        text,
                        long_press_text,
                    } => {
                        self.page_handle = sys::nbgl_pageDrawGenericContent(
                            Some(layout_touch_callback),
                            navigation_info,
                            &mut sys::nbgl_pageContent_t {
                                title: self.alloc_cstring(page_content_info.title.as_ref())?,
                                isTouchableTitle: false, // unused in nbgl
                                titleToken: TOKEN_TITLE,
                                tuneId: 0,
                                topRightToken: 255,              // not implemented
                                topRightIcon: core::ptr::null(), // not implemented
                                type_: sys::INFO_LONG_PRESS,
                                __bindgen_anon_1: sys::nbgl_pageContent_s__bindgen_ty_1 {
                                    infoLongPress: sys::nbgl_contentInfoLongPress_t {
                                        text: self.alloc_cstring(Some(text))?,
                                        icon: core::ptr::null(),
                                        longPressText: self.alloc_cstring(Some(long_press_text))?,
                                        longPressToken: TOKEN_CONFIRM,
                                        tuneId: 0,
                                    },
                                },
                            },
                        );
                    }
                };
            },
            common::ux::Page::Home { description } => unsafe {
                self.release_handle();
                self.clear_cstrings();

                let ticker_config = sys::nbgl_screenTickerConfiguration_t {
                    tickerCallback: None, // we could put a callback here if we had a timer
                    tickerValue: 0,       // no timer
                    tickerIntervale: 0,   // not periodic
                };

                let page_info = sys::nbgl_pageInfoDescription_t {
                    centeredInfo: sys::nbgl_contentCenteredInfo_t {
                        text1: self.alloc_cstring(Some(description))?,
                        text2: core::ptr::null(),
                        text3: core::ptr::null(),
                        icon: core::ptr::null(),
                        onTop: false,
                        style: sys::LARGE_CASE_INFO,
                        offsetY: 0,
                    },
                    topRightStyle: sys::INFO_ICON,
                    bottomButtonStyle: sys::QUIT_APP_TEXT,
                    topRightToken: TOKEN_TOPRIGHT,
                    bottomButtonsToken: TOKEN_QUIT,
                    footerText: core::ptr::null(),
                    footerToken: 0,
                    tapActionText: core::ptr::null(),
                    isSwipeable: true,
                    tapActionToken: 0,
                    actionButtonText: core::ptr::null(),
                    actionButtonIcon: core::ptr::null(),
                    actionButtonStyle: sys::BLACK_BACKGROUND,
                    tuneId: sys::TUNE_TAP_CASUAL,
                };

                self.page_handle = sys::nbgl_pageDrawInfo(
                    Some(layout_touch_callback),
                    &ticker_config, // or core::ptr::null()
                    &page_info,
                );
            },
        }

        unsafe {
            sys::nbgl_refresh();
        }
        Ok(())
    }

    #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
    pub fn show_step(&mut self, _step: &Step) -> Result<(), CommEcallError> {
        Err(CommEcallError::UnhandledEcall)
    }

    #[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
    pub fn show_step(&mut self, step: &Step) -> Result<(), CommEcallError> {
        match step {
            Step::TextSubtext {
                pos,
                text,
                subtext,
                style,
            } => {
                self.release_handle();
                self.clear_cstrings();
                unsafe {
                    self.step_handle = sys::nbgl_stepDrawText(
                        *pos,
                        Some(step_button_callback), // callback
                        core::ptr::null_mut(),      // ticker (todo)
                        self.alloc_cstring(Some(text))?,
                        self.alloc_cstring(Some(subtext))?,
                        *style, // style
                        false,  // not modal
                    );
                }

                Ok(())
            }
            Step::CenteredInfo {
                pos,
                text,
                subtext,
                icon,
                style,
            } => {
                self.release_handle();
                self.clear_cstrings();

                unsafe {
                    self.step_handle = sys::nbgl_stepDrawCenteredInfo(
                        *pos,
                        Some(step_button_callback), // callback
                        core::ptr::null_mut(),      // ticker (todo)
                        &mut sys::nbgl_layoutCenteredInfo_t {
                            icon: icon.to_icon_details(),
                            text1: self.alloc_cstring(text.as_ref())?,
                            text2: self.alloc_cstring(subtext.as_ref())?,
                            onTop: false,
                            style: *style,
                            ..Default::default()
                        }, // info
                        false,                      // not modal
                    );
                }

                Ok(())
            }
        }
    }
}
