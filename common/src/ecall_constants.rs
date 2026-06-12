pub const ECALL_FATAL: u32 = 1;
pub const ECALL_XSEND: u32 = 2;
pub const ECALL_XRECV: u32 = 3;
pub const ECALL_EXIT: u32 = 4;
pub const ECALL_PRINT: u32 = 5;

// device handling, events, and UX

pub const ECALL_GET_EVENT: u32 = 10;
pub const ECALL_GET_DEVICE_PROPERTY: u32 = 15;

// Display ECALLs. The block 40..=63 is dedicated to display operations.

// Low-level graphics: blit a rectangle of pixels from guest memory to the screen.
pub const ECALL_DISPLAY_BLIT: u32 = 40;
// Low-level graphics: push a previously drawn rectangle to the physical panel.
pub const ECALL_DISPLAY_REFRESH: u32 = 41;
// Accelerated drawing: fill a rectangle with a solid palette color directly in the
// OS framebuffer (no guest framebuffer, no per-pixel work). See `Color`.
pub const ECALL_DISPLAY_FILL_RECT: u32 = 42;
// Accelerated drawing: draw a UTF-8 string with an OS font directly in the framebuffer.
pub const ECALL_DISPLAY_DRAW_TEXT: u32 = 43;
// Text measurement with an OS font, so a UI can lay out text without rasterizing it in
// the guest. `text_width` returns the rendered width in pixels of a UTF-8 string;
// `font_metrics` returns packed `(height << 16) | line_height` for a `Font`.
pub const ECALL_DISPLAY_TEXT_WIDTH: u32 = 44;
pub const ECALL_DISPLAY_FONT_METRICS: u32 = 45;
// 46..=63 are reserved for future display ops (icons, lines, rounded rects, QR codes,
// compressed images, ...). Note: rounded-rect / QR / icon ops need NBGL functions
// (`nbgl_drawRoundedRect`, `nbgl_drawQrCode`, `nbgl_drawIcon` in `nbgl_draw.c`) that are
// neither BOLOS syscalls nor compiled into the VM, so they are not available yet; only
// syscall-backed primitives (`nbgl_frontDrawRect`, `nbgl_drawText`) are exposed for now.

// Constants used for GET_DEVICE_PROPERTY

// device id (vendor_id: u16, product_id: u16)
pub const DEVICE_PROPERTY_ID: u32 = 0x01;
// (screen_width: u16, screen_height: u16)
pub const DEVICE_PROPERTY_SCREEN_SIZE: u32 = 0x02;
// bitmask of device features (to be defined)
pub const DEVICE_PROPERTY_FEATURES: u32 = 0x03;
// the device's native pixel format (a `PixelFormat` value), used for `display_blit`
pub const DEVICE_PROPERTY_PIXEL_FORMAT: u32 = 0x04;

/// Pixel format of a buffer passed to the `display_blit` ECALL.
///
/// In every format, rows are stored top-to-bottom, each row padded to a whole
/// number of bytes (see [`PixelFormat::stride`]).
///
/// Encodings start at 1: in every ABI enum, 0 is reserved as invalid/unknown
/// (so e.g. `get_device_property` can use 0 for "unsupported property").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum PixelFormat {
    /// 1 bit per pixel: 0 = black, 1 = white. Each row is MSB-first
    /// (the leftmost pixel is the most significant bit of the first byte) and
    /// padded to a byte boundary: `stride = (w + 7) / 8`.
    Mono1 = 1,
    /// 4 bits per pixel grayscale, 0 = black ..= 15 = white. Two pixels per byte,
    /// the high nibble being the left pixel; rows are padded to a byte boundary:
    /// `stride = (w + 1) / 2`.
    Gray4 = 2,
}

impl PixelFormat {
    /// Reconstructs a `PixelFormat` from its `u32` ECALL encoding.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(PixelFormat::Mono1),
            2 => Some(PixelFormat::Gray4),
            _ => None,
        }
    }

    /// Number of bits used to encode a single pixel.
    pub const fn bits_per_pixel(self) -> usize {
        match self {
            PixelFormat::Mono1 => 1,
            PixelFormat::Gray4 => 4,
        }
    }

    /// Number of bytes used to encode a single row of `width` pixels, padded to a
    /// whole number of bytes.
    pub const fn stride(self, width: usize) -> usize {
        (width * self.bits_per_pixel() + 7) / 8
    }

    /// Total number of bytes required to encode a `width` × `height` image.
    pub const fn buffer_len(self, width: usize, height: usize) -> usize {
        self.stride(width) * height
    }
}

/// A color in NBGL's 4-color palette, used by the accelerated vector drawing
/// ECALLs (`display_fill_rect`, `display_draw_line`, `display_draw_text`, …).
///
/// These ops are drawn natively in the OS framebuffer and only carry a tiny
/// descriptor across the ECALL boundary, so they are vastly cheaper than rendering
/// pixels in guest RAM and blitting. The trade-off is that they offer only the four
/// palette colors; for full 16-level grayscale content (gradients, photos), use the
/// `display_blit` path with [`PixelFormat::Gray4`].
///
/// The numeric values match the Ledger SDK `color_t` palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Color {
    Black = 0,
    DarkGray = 1,
    LightGray = 2,
    White = 3,
}

impl Color {
    /// Reconstructs a `Color` from its `u32` ECALL encoding.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Color::Black),
            1 => Some(Color::DarkGray),
            2 => Some(Color::LightGray),
            3 => Some(Color::White),
            _ => None,
        }
    }

    /// The equivalent 4bpp grayscale intensity (`0..=15`), matching the SDK's
    /// `EXPAND_TO_4BPP` mapping (`(c << 2) | c`): Black=0, DarkGray=5, LightGray=10,
    /// White=15. Used by the native/emulator backend.
    pub const fn intensity(self) -> u8 {
        let c = self as u8;
        (c << 2) | c
    }
}

/// Panel refresh mode for the `display_refresh` ECALL.
///
/// The panel refresh is the expensive part of putting something on an e-ink screen;
/// picking a cheaper mode for small or monochrome updates is a major performance lever.
/// Modes are named for intent, not for any specific panel technology; on Ledger
/// devices they map to the SDK's `nbgl_refresh_mode_t` values.
///
/// Encodings start at 1: 0 is reserved as invalid/unknown in every ABI enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum RefreshMode {
    /// The best quality the panel offers: slowest. Sensible default on grayscale
    /// (`Gray4`) devices.
    FullQuality = 1,
    /// Localized update, quality maintained — for small changes (toggles, a status line).
    Partial = 2,
    /// Pure black & white refresh, contrast prioritized. Default on monochrome (`Mono1`)
    /// devices.
    Mono = 3,
    /// Pure black & white refresh, speed prioritized over contrast.
    MonoFast = 4,
}

impl RefreshMode {
    /// Reconstructs a `RefreshMode` from its `u32` ECALL encoding.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(RefreshMode::FullQuality),
            2 => Some(RefreshMode::Partial),
            3 => Some(RefreshMode::Mono),
            4 => Some(RefreshMode::MonoFast),
            _ => None,
        }
    }
}

/// A semantic font selector for the `display_draw_text` ECALL.
///
/// Fonts are device-specific bitmap assets baked into the OS; rather than expose raw
/// per-device font ids, V-Apps pick a semantic role and the VM maps it to the right
/// `nbgl_font_id_e` for the current device.
///
/// Encodings start at 1: 0 is reserved as invalid/unknown in every ABI enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Font {
    /// The standard body/regular text font.
    Regular = 1,
    /// The bold/semibold text font.
    Bold = 2,
    /// The large/title font.
    Large = 3,
}

impl Font {
    /// Reconstructs a `Font` from its `u32` ECALL encoding.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Font::Regular),
            2 => Some(Font::Bold),
            3 => Some(Font::Large),
            _ => None,
        }
    }

    /// A stable 0-based index for table lookups (`Regular` = 0, `Bold` = 1, `Large` = 2).
    pub const fn index(self) -> usize {
        self as usize - 1
    }
}

// Persistent storage
pub const ECALL_STORAGE_READ: u32 = 20;
pub const ECALL_STORAGE_WRITE: u32 = 21;

// Big numbers
pub const ECALL_MODM: u32 = 110;
pub const ECALL_ADDM: u32 = 111;
pub const ECALL_SUBM: u32 = 112;
pub const ECALL_MULTM: u32 = 113;
pub const ECALL_POWM: u32 = 114;
pub const ECALL_MODINV_PRIME: u32 = 115;

pub const MAX_BIGNUMBER_SIZE: usize = 64;

// HD derivations
pub enum CurveKind {
    Secp256k1 = 0x21,
}

// TODO: IDs for now are matching the ones in the ledger SDK
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum HashId {
    Ripemd160 = 1,
    Sha256 = 3,
    Sha512 = 5,
    Keccak = 6,
    Sha3 = 7,
}

impl HashId {
    /// Returns the composite ECALL hash_id passed to hash_init / hash_update / hash_final.
    ///
    /// Layout (32 bits):
    ///   - bits 31-24: always 0 (reserved for future extensibility)
    ///   - bits 23-16: algorithm identifier (u8, matches the Ledger SDK hash type constants)
    ///   - bits 15-0:  output size in bytes, supplied by the caller
    ///
    /// Casting through `u8` enforces that the algorithm identifier always fits in a single byte,
    /// keeping the high 8 bits of the hash_id parameter free for future use.
    /// The output size is passed explicitly rather than being derived from `self` so that
    /// one algorithm identifier can support multiple output lengths.
    /// Note that only certain output sizes might be valid for each algorithm.
    pub const fn ecall_id(self, output_size: u16) -> u32 {
        ((self as u8 as u32) << 16) | (output_size as u32)
    }
}

// TODO: signing modes for now are matching the ones in the ledger SDK
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum EcdsaSignMode {
    RFC6979 = (3 << 9),
}

// TODO: signing modes for now are matching the ones in the ledger SDK
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum SchnorrSignMode {
    BIP340 = 0,
}

pub const ECALL_DERIVE_HD_NODE: u32 = 130;
pub const ECALL_GET_MASTER_FINGERPRINT: u32 = 131;
pub const ECALL_DERIVE_SLIP21_KEY: u32 = 132;

// Hash functions
pub const ECALL_HASH_INIT: u32 = 150;
pub const ECALL_HASH_UPDATE: u32 = 151;
pub const ECALL_HASH_DIGEST: u32 = 152;

/// Size in bytes of the hash context structs used by the hash ecalls.
///
/// These are the sizes of the opaque context buffers passed to `hash_init`,
/// `hash_update`, and `hash_final`. Each context must be at least this large.
/// The sizes are at least 16 bytes larger than what the Ledger OS uses, in
/// order to have some leeway for different targets where the struct might
/// be larger.
pub const CTX_SHA256_SIZE: usize = 128;
pub const CTX_SHA512_SIZE: usize = 224;
pub const CTX_RIPEMD160_SIZE: usize = 120;
// Keccak and SHA-3 share the same internal Keccak-f[1600] state (cx_sha3_t on Ledger, ~200 bytes
// of rate buffer + 25×u64 state). 448 bytes gives comfortable headroom above the 424-byte
// cx_sha3_t and the RustCrypto sha3 context structs.
pub const CTX_SHA3_SIZE: usize = 448;

// Operations for public keys over elliptic curves
pub const ECALL_ECFP_ADD_POINT: u32 = 160;
pub const ECALL_ECFP_SCALAR_MULT: u32 = 161;

// Random number generation
pub const ECALL_GET_RANDOM_BYTES: u32 = 170;

// Signatures
pub const ECALL_ECDSA_SIGN: u32 = 180;
pub const ECALL_ECDSA_VERIFY: u32 = 181;
pub const ECALL_SCHNORR_SIGN: u32 = 182;
pub const ECALL_SCHNORR_VERIFY: u32 = 183;

/// =======================================
/// Device-specific ECALLs
/// =======================================
/// The range 192..255 is reserved for vendor-specific ECALLs
/// Different implementation of Vanadium can assign different meaning to these ECALLs.
/// The following ECALLs are defined for Ledger devices.

pub const ECALL_SHOW_PAGE: u32 = 192; // Flex / Stax / Apex_P
pub const ECALL_SHOW_STEP: u32 = 193; // Nano X / Nano S+

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{collections::BTreeMap, format, string::String, vec::Vec};

    // Extracts every `pub const ECALL_*: u32 = <literal>;` from this file's source, so
    // the test cannot go stale when an ECALL is added. A constant whose value is not a
    // plain decimal/hex literal fails loudly: rewrite it as a literal (the ECALL table
    // is an ABI, its numbers should be readable at a glance).
    fn all_ecall_constants() -> Vec<(String, u32)> {
        let src = include_str!("ecall_constants.rs");
        let mut found = Vec::new();
        for line in src.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ECALL_") else {
                continue;
            };
            let (name, rest) = rest.split_once(':').expect("malformed ECALL constant");
            let value = rest
                .split_once('=')
                .expect("malformed ECALL constant")
                .1
                .split(';')
                .next()
                .unwrap()
                .trim();
            let value = match value.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => value.parse(),
            }
            .expect("ECALL value is not a plain integer literal");
            found.push((format!("ECALL_{}", name.trim()), value));
        }
        found
    }

    /// Two ECALLs once shipped with the same number (display_text_width and
    /// storage_read, both 20): the dispatcher's second match arm was silently
    /// unreachable. This test makes any future collision fail in CI.
    #[test]
    fn ecall_numbers_are_unique() {
        let ecalls = all_ecall_constants();
        // If parsing breaks (e.g. the constants move to another file), fail rather
        // than silently checking nothing.
        assert!(ecalls.len() >= 30, "only {} ECALL constants parsed", ecalls.len());

        let mut by_value: BTreeMap<u32, String> = BTreeMap::new();
        for (name, value) in ecalls {
            if let Some(previous) = by_value.insert(value, name.clone()) {
                panic!("ECALL number collision: {previous} and {name} are both {value}");
            }
        }
    }
}
