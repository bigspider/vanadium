//! A self-test of the display ECALL ABI contract, run *inside* a V-App against
//! whatever implements the ECALLs underneath (the VM on device/Speculos, or the
//! native backend) — so the integration suite can assert the exact error codes and
//! conventions on the real dispatch path, not only against the native reference.
//!
//! Hidden from docs: this is test support, not app API. It draws a few pixels into
//! the framebuffer (without refreshing more than tiny rectangles), so run it on a
//! screen whose content doesn't matter.

use common::ecall_constants::*;

use crate::ecalls;

/// Runs every check and returns a bitmask of the **failed** ones (0 = contract
/// holds). Bit `i` set means check `i` failed; the numbering is the order below,
/// so a failure pinpoints the violated rule.
pub fn display_abi_probe() -> u64 {
    let size = ecalls::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE);
    let (w, h) = display_unpack_pair(size);
    let g = DisplayGranularity::from_u32(ecalls::get_device_property(
        DEVICE_PROPERTY_DISPLAY_GRANULARITY,
    ));

    let buf = [0u8; 64];
    let txt = b"probe";
    let long = [b'a'; DISPLAY_MAX_TEXT_LEN + 1];
    let pos00 = display_pack_pair(0, 0);
    let box_16 = display_pack_pair(64, 16);
    let font = Font::Regular as u32;

    // A blit whose destination violates the advertised granularity must fail with
    // ALIGNMENT — derivable only when the device has a nontrivial constraint.
    let alignment_check = match g {
        Some(g) if g.y > 1 => unsafe {
            ecalls::display_blit(
                display_pack_pair(0, 1),
                display_pack_pair(1, g.h as u16),
                buf.as_ptr(),
                buf.len(),
                0,
                1,
                PixelFormat::Gray4 as u32,
            ) == DISPLAY_ERR_ALIGNMENT
        },
        _ => true,
    };

    let checks: &[bool] = &[
        // --- device properties: every defined one nonzero; unknown ones 0 ---
        /*  0 */ ecalls::get_device_property(DEVICE_PROPERTY_ID) != 0,
        /*  1 */ size != 0 && w > 0 && h > 0,
        /*  2 */ ecalls::get_device_property(DEVICE_PROPERTY_FEATURES) != 0,
        /*  3 */
        PixelFormat::from_u32(ecalls::get_device_property(DEVICE_PROPERTY_PIXEL_FORMAT)).is_some(),
        /*  4 */ g.is_some(),
        /*  5 */ ecalls::get_device_property(DEVICE_PROPERTY_MAX_TEXT_LEN) != 0,
        /*  6 */ ecalls::get_device_property(DEVICE_PROPERTY_ABI_REVISION) != 0,
        /*  7 */ ecalls::get_device_property(0x7fff_ffff) == 0,
        // --- display_blit ---
        /*  8 */
        unsafe { ecalls::display_blit(pos00, display_pack_pair(2, 4), buf.as_ptr(), 4, 0, 1, 0) }
            == DISPLAY_ERR_INVALID_ARG,
        /*  9 */
        unsafe { ecalls::display_blit(pos00, display_pack_pair(2, 4), buf.as_ptr(), 4, 0, 1, 99) }
            == DISPLAY_ERR_UNSUPPORTED,
        /* 10 */
        unsafe {
            ecalls::display_blit(
                display_pack_pair(w as u16, 0),
                display_pack_pair(1, 4),
                buf.as_ptr(),
                buf.len(),
                0,
                1,
                PixelFormat::Gray4 as u32,
            )
        } == DISPLAY_ERR_OUT_OF_BOUNDS,
        /* 11 */ alignment_check,
        /* 12 */
        unsafe {
            // A 2x4 Gray4 rect at stride 1 addresses 4 bytes; 3 readable is too few.
            ecalls::display_blit(
                pos00,
                display_pack_pair(2, 4),
                buf.as_ptr(),
                3,
                0,
                1,
                PixelFormat::Gray4 as u32,
            )
        } == DISPLAY_ERR_BAD_LAYOUT,
        /* 13 */
        unsafe {
            ecalls::display_blit(pos00, 0, buf.as_ptr(), 0, 0, 0, PixelFormat::Gray4 as u32)
        } == 0,
        /* 14 */
        unsafe {
            // Every defined format draws on every device (converted if non-native).
            ecalls::display_blit(
                pos00,
                display_pack_pair(8, 4),
                buf.as_ptr(),
                4,
                0,
                1,
                PixelFormat::Mono1 as u32,
            )
        } == 0,
        /* 15 */
        unsafe {
            ecalls::display_blit(
                pos00,
                display_pack_pair(2, 4),
                buf.as_ptr(),
                4,
                0,
                1,
                PixelFormat::Gray4 as u32,
            )
        } == 0,
        // --- display_refresh: bad modes fail, everything else succeeds ---
        /* 16 */
        unsafe { ecalls::display_refresh(pos00, display_pack_pair(4, 4), 0) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 17 */
        unsafe { ecalls::display_refresh(pos00, display_pack_pair(4, 4), 99) }
            == DISPLAY_ERR_UNSUPPORTED,
        /* 18 */
        unsafe {
            ecalls::display_refresh(pos00, display_pack_pair(4, 4), RefreshMode::FullQuality as u32)
        } == 0,
        /* 19 */
        unsafe {
            ecalls::display_refresh(pos00, display_pack_pair(4, 4), RefreshMode::Partial as u32)
        } == 0,
        /* 20 */
        unsafe { ecalls::display_refresh(pos00, display_pack_pair(4, 4), RefreshMode::Mono as u32) }
            == 0,
        /* 21 */
        unsafe {
            ecalls::display_refresh(pos00, display_pack_pair(4, 4), RefreshMode::MonoFast as u32)
        } == 0,
        /* 22 */
        unsafe {
            // The rectangle is advisory: off-screen clips to a no-op success.
            ecalls::display_refresh(
                display_pack_pair(0xffff, 0xffff),
                display_pack_pair(8, 8),
                RefreshMode::FullQuality as u32,
            )
        } == 0,
        // --- display_fill_rect: colors quantize, only reserved bits fail ---
        /* 23 */
        unsafe { ecalls::display_fill_rect(pos00, display_pack_pair(4, 4), 0x0100_0000) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 24 */
        unsafe {
            ecalls::display_fill_rect(
                display_pack_pair(0, h as u16),
                display_pack_pair(4, 4),
                0xffffff,
            )
        } == DISPLAY_ERR_OUT_OF_BOUNDS,
        /* 25 */
        unsafe { ecalls::display_fill_rect(pos00, display_pack_pair(4, 4), 0x123456) } == 0,
        /* 26 */ unsafe { ecalls::display_fill_rect(pos00, 0, 0xffffff) } == 0,
        // --- display_draw_text ---
        /* 27 */
        unsafe { ecalls::display_draw_text(pos00, box_16, txt.as_ptr(), txt.len(), 0, 0, 0xffffff) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 28 */
        unsafe {
            ecalls::display_draw_text(pos00, box_16, txt.as_ptr(), txt.len(), 99, 0, 0xffffff)
        } == DISPLAY_ERR_UNSUPPORTED,
        /* 29 */
        unsafe {
            ecalls::display_draw_text(
                pos00,
                box_16,
                txt.as_ptr(),
                txt.len(),
                font,
                0x0100_0000,
                0xffffff,
            )
        } == DISPLAY_ERR_INVALID_ARG,
        /* 30 */
        unsafe {
            ecalls::display_draw_text(
                pos00,
                box_16,
                txt.as_ptr(),
                txt.len(),
                font,
                0,
                0x0100_0000,
            )
        } == DISPLAY_ERR_INVALID_ARG,
        /* 31 */
        unsafe {
            ecalls::display_draw_text(
                display_pack_pair(0, h as u16),
                box_16,
                txt.as_ptr(),
                txt.len(),
                font,
                0,
                0xffffff,
            )
        } == DISPLAY_ERR_OUT_OF_BOUNDS,
        /* 32 */
        unsafe {
            ecalls::display_draw_text(pos00, box_16, long.as_ptr(), long.len(), font, 0, 0xffffff)
        } == DISPLAY_ERR_TOO_LONG,
        /* 33 */
        unsafe { ecalls::display_draw_text(pos00, box_16, b"\xff\xfe".as_ptr(), 2, font, 0, 0xffffff) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 34 */
        unsafe { ecalls::display_draw_text(pos00, box_16, b"a\0b".as_ptr(), 3, font, 0, 0xffffff) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 35 */
        unsafe { ecalls::display_draw_text(pos00, 0, txt.as_ptr(), txt.len(), font, 0, 0xffffff) }
            == 0,
        /* 36 */
        unsafe {
            ecalls::display_draw_text(pos00, box_16, txt.as_ptr(), txt.len(), font, 0, 0xffffff)
        } == 0,
        // --- display_text_width ---
        /* 37 */
        unsafe { ecalls::display_text_width(0, txt.as_ptr(), txt.len()) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 38 */
        unsafe { ecalls::display_text_width(99, txt.as_ptr(), txt.len()) }
            == DISPLAY_ERR_UNSUPPORTED,
        /* 39 */
        unsafe { ecalls::display_text_width(font, long.as_ptr(), long.len()) }
            == DISPLAY_ERR_TOO_LONG,
        /* 40 */
        unsafe { ecalls::display_text_width(font, b"a\0b".as_ptr(), 3) }
            == DISPLAY_ERR_INVALID_ARG,
        /* 41 */ unsafe { ecalls::display_text_width(font, txt.as_ptr(), 0) } == 0,
        /* 42 */ unsafe { ecalls::display_text_width(font, txt.as_ptr(), txt.len()) } > 0,
        // --- display_font_metrics ---
        /* 43 */ unsafe { ecalls::display_font_metrics(0) } == DISPLAY_ERR_INVALID_ARG,
        /* 44 */ unsafe { ecalls::display_font_metrics(99) } == DISPLAY_ERR_UNSUPPORTED,
        /* 45 */
        {
            let m = unsafe { ecalls::display_font_metrics(font) };
            m > 0 && (m >> 16) > 0 && (m & 0xffff) > 0
        },
    ];

    let mut failed = 0u64;
    for (i, &ok) in checks.iter().enumerate() {
        if !ok {
            failed |= 1 << i;
        }
    }
    failed
}
