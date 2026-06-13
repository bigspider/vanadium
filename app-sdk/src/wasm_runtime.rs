//! Runtime helpers for driving a V-App from a web page (`target_wasm`, architecture A).
//!
//! A V-App's wasm module re-exports these (with `#[no_mangle]`) so the page can paint the
//! framebuffer to a `<canvas>`. The framebuffer is the same one the display ECALLs draw
//! into; `App::dispatch_blocking` runs a command, the display ops update it, and the page
//! reads it here.

/// Pointer to the framebuffer (one byte per pixel, intensity 0..=15), in wasm memory.
/// Stable for the life of the module.
pub fn framebuffer_ptr() -> *const u8 {
    crate::ecalls_wasm::framebuffer_ptr()
}

/// Framebuffer width in pixels.
pub fn framebuffer_width() -> usize {
    crate::ecalls_wasm::framebuffer_dims().0
}

/// Framebuffer height in pixels.
pub fn framebuffer_height() -> usize {
    crate::ecalls_wasm::framebuffer_dims().1
}

/// A counter bumped on every `display_refresh`, so the page can repaint only on change.
pub fn framebuffer_version() -> u64 {
    crate::ecalls_wasm::framebuffer_version()
}
