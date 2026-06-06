# Low-level graphics ECALLs

> Status: **experimental / proof-of-concept**. The blit ECALL, its pixel formats,
> and the SDK `Canvas` abstraction are subject to change.

## Motivation

Today the only UI primitives a V-App can use are the high-level, Ledger-specific
`show_page` / `show_step` ECALLs (see [`common/src/ux`](../common/src/ux)). These
serialize a fixed catalog of NBGL widgets (`Page` / `Step`) and are rendered on
device by Ledger's NBGL widget library. This is convenient for transaction-review
style flows but:

- it is tightly coupled to NBGL and the Ledger device line;
- the set of available widgets is fixed in the VM;
- a V-App cannot draw anything that is not already a predefined widget.

The goal of the graphics ECALLs is to give V-Apps a **general-purpose, framework
-agnostic** way to put pixels on the screen, so that standard Rust embedded UI
frameworks such as [`embedded-graphics`](https://docs.rs/embedded-graphics) can be
used on top of the Vanadium SDK.

## Design principle: draw in the guest, blit to the screen

The key observation is that `embedded-graphics` (and similar frameworks) only need
a `DrawTarget` that can set pixels in a buffer. If that buffer lives in **guest
RAM**, then *all* of the rendering work — lines, text, shapes, the entire
framework — runs as ordinary RISC-V code inside the VM, with **zero ECALL
overhead**. The only operation that must cross the ECALL boundary is pushing a
finished (sub-)rectangle of pixels to the physical screen.

This makes the new ECALL surface essentially a single primitive: a **blit**.

```
            guest RAM                         ECALL boundary            device
   ┌───────────────────────────┐                  │
   │  Canvas (framebuffer)      │                  │
   │  + embedded-graphics       │  display_blit()  │     NBGL front buffer
   │    DrawTarget  ──────────► │ ───────────────► │ ──► (nbgl_frontDrawImage)
   └───────────────────────────┘                  │         + refresh
```

Contrast with the rejected alternative of exposing per-primitive drawing ECALLs
(`draw_rect`, `draw_pixel`, `draw_text`, …) that map onto NBGL primitives:

- `embedded-graphics` would emit *one ECALL per pixel run*, which is catastrophically
  chatty across the VM boundary;
- it would permanently couple the stable ECALL surface to NBGL's primitive set.

The blit model keeps the stable surface tiny and lets the SDK abstraction evolve
(including adding accelerated ops later) without ECALL changes.

## The blit ECALL

```rust
// common/src/ecall_constants.rs
pub const ECALL_DISPLAY_BLIT: u32 = 12;

display_blit(
    x: u32, y: u32,        // top-left of the destination rectangle, in screen pixels
    w: u32, h: u32,        // size of the rectangle, in pixels
    buffer: *const u8,     // pixel data, in guest memory
    buffer_len: usize,     // length of `buffer` in bytes
    format: u32,           // a PixelFormat value describing `buffer`
) -> u32                   // 1 on success, 0 on error
```

7 arguments fit comfortably in the `a0..a7` ECALL ABI.

The VM handler:

1. validates that `[x, x+w) × [y, y+h)` lies within the device screen
   (`DEVICE_PROPERTY_SCREEN_SIZE`);
2. validates that `buffer_len == stride(format, w) * h`;
3. reads the rectangle out of guest memory in **horizontal bands** (the
   outsourced/paged `read_buffer` crosses page boundaries transparently), and for
   each band draws one `nbgl_frontDrawImage`, then refreshes the dirty rectangle.

The band layout is dictated by two NBGL/hardware constraints that are easy to miss
(they are documented in the SDK but not enforced by the Speculos reference driver,
so violating them renders fine in the emulator and wrong on a real device):

- **`y0` and `height` must be multiples of 4.** So bands are aligned to 4 rows; the
  SDK expands `flush_area` regions to satisfy this. The smallest band is 4 rows.
- **`nbgl_frontDrawImage` consumes its buffer column-major**, right-to-left,
  top-to-bottom, packed MSB-first (high nibble first for 4bpp) with no per-column
  padding (see `nbgl_driver_drawImage`). The SDK framebuffer is row-major, so the VM
  **transposes** each band into this layout before drawing. (This column-major,
  right-to-left consumption — not any mirror transformation — is also what makes a
  naively row-major buffer come out horizontally reversed.)

Band scratch is sized for the minimum (4 rows) and allocated once and reused, since
the VM heap is tiny (~24 KB, mostly page caches); `start_vapp` reserves a small
fixed amount of heap (`ECALL_SCRATCH_RESERVE`) from the caches for it.

Because the caller chooses `x/y/w/h`, partial updates ("dirty rectangles") are
natural and are the main tool for keeping the guest→SE data transfer cheap (a full
Stax 4bpp frame is ~134 KB; you rarely want to push all of it every refresh).

## Pixel formats

```rust
// common/src/ecall_constants.rs
#[repr(u32)]
pub enum PixelFormat {
    /// 1 bit per pixel. 0 = background, 1 = foreground.
    /// Rows are MSB-first and padded to a byte boundary: stride = (w + 7) / 8.
    Mono1 = 0,
    /// 4 bits per pixel grayscale. 0 = black .. 15 = white.
    /// Two pixels per byte, the high nibble is the left pixel; rows are padded to a
    /// byte boundary: stride = (w + 1) / 2.
    Gray4 = 1,
}
```

These two formats are chosen to match the native NBGL bit depths exactly and avoid
conversion cost:

| Device           | Screen      | Native format |
|------------------|-------------|---------------|
| Nano X / S+      | 128×64      | `Mono1`       |
| Stax             | 400×672     | `Gray4`       |
| Flex             | 480×600     | `Gray4`       |
| Apex P           | 300×400     | `Gray4`       |

A new device property advertises the native format so the SDK can pick the right
`Canvas` automatically:

```rust
pub const DEVICE_PROPERTY_PIXEL_FORMAT: u32 = 0x04; // returns a PixelFormat value
```

A color format (e.g. `Rgb565`) can be added later if a color device appears; it
only requires a new `PixelFormat` variant plus VM-side handling.

## Input

Raw graphics needs raw input, not the semantic `Action`s (Confirm/Reject/…) that
`show_page` produces. The existing `EventData` union already reserves a 16-byte
`raw` field, and `get_event` already has an `Event::Unknown([u8; 16])` path, so no
new ECALL is needed — only new `EventCode` variants decoded by the SDK:

- touch devices (Stax/Flex/Apex): `Touch { x: u16, y: u16, state: Pressed|Released }`
- button devices (Nano): `Button { which, state }`

This part is **not** implemented in the current proof-of-concept (the demo is
output-only); it is the natural follow-up.

## Screen ownership

Raw blits and the NBGL page/step UX both target the same physical screen and are
mutually exclusive. The contract for now is simple: **once a V-App blits, it owns
the screen** and must not interleave `show_page` / `show_step`. An explicit
enter/leave-graphics-mode ECALL can be added later if a cleaner hand-off is needed
(e.g. to restore the dashboard).

## SDK abstraction

The raw ECALL stays private (per [`docs/ecalls.md`](./ecalls.md)); V-Apps use a
clean `Canvas` type in the app-sdk:

```rust
use vanadium_app_sdk::ux::canvas::Canvas;

let mut canvas = Canvas::new_for_device();   // picks size + native PixelFormat

// `Canvas` implements embedded_graphics::DrawTarget<Color = Gray4>
// (behind the `embedded-graphics` feature):
Rectangle::new(Point::new(10, 10), Size::new(60, 40))
    .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
    .draw(&mut canvas)?;

canvas.flush();                  // blit the whole frame
canvas.flush_area(dirty_rect);   // or just a sub-rectangle
```

- `Canvas` owns a framebuffer in guest RAM, stored **packed** in the device's
  native `PixelFormat` (4bpp for `Gray4`, 1bpp for `Mono1`), and exposes a raw
  `set_pixel` API that is always available. Packing keeps a full Flex screen at
  ~144 KB (vs. 288 KB at one byte per pixel) and lets a full-width flush blit the
  backing buffer with no extra allocation.
- **Heap sizing:** the framebuffer lives in the V-App heap, which defaults to
  64 KB (`VAPP_HEAP_SIZE`). A full-screen canvas needs a larger heap — e.g. the
  `sadik` demo sets `VAPP_HEAP_SIZE=262144`. Apps that only need a small region
  can keep the default by creating a smaller `Canvas` and blitting it at an offset.
- The `embedded_graphics::DrawTarget` + `OriginDimensions` impls are gated behind
  the optional `embedded-graphics` feature so the dependency is opt-in.
- `flush` / `flush_area` are the only methods that issue the blit ECALL.

Because apps only see `Canvas`, the underlying ECALL (or the device pixel format,
or even a future switch to accelerated drawing ops) can change without breaking
app code.

## Numbering & stabilization

Unlike `show_page` / `show_step` (which are genuinely Ledger-specific and live in
the vendor range 192–255), the blit concept is generic and target-portable, so
`ECALL_DISPLAY_BLIT` lives in the general "device handling, events, and UX" block.
This positions low-level graphics as part of the **core** ECALL set the project
intends to stabilize.

## Native / emulator backend

On the native target the "screen" was previously the terminal (text). For
graphics, `display_blit` updates an in-memory virtual framebuffer and:

- by default, dumps it to a `vapp_screen.ppm` file (no system dependencies, works
  in CI / unit tests);
- behind the optional `gui` feature, mirrors it to an
  [`embedded-graphics-simulator`](https://docs.rs/embedded-graphics-simulator)
  SDL window for pixel-accurate parity with on-device output.

## Implementation checklist (per docs/ecalls.md)

- [x] constants in [`common/src/ecall_constants.rs`](../common/src/ecall_constants.rs)
- [x] raw ECALL codegen in [`ecalls/src/ecalls_impl.rs`](../ecalls/src/ecalls_impl.rs)
- [x] trait declaration in [`app-sdk/src/ecalls.rs`](../app-sdk/src/ecalls.rs)
- [x] riscv delegate in [`app-sdk/src/ecalls_riscv.rs`](../app-sdk/src/ecalls_riscv.rs)
- [x] native impl in [`app-sdk/src/ecalls_native.rs`](../app-sdk/src/ecalls_native.rs)
- [x] SDK abstraction: `ux::canvas::Canvas`
- [x] VM handler in [`vm/src/handlers/lib/ecall.rs`](../vm/src/handlers/lib/ecall.rs)
      and the unified NBGL draw in
      [`ux_handler.rs`](../vm/src/handlers/lib/ecall/ux_handler.rs) — one
      format-aware `blit_row`/`blit_refresh` drives every model (Gray4 on
      stax/flex/apex_p, Mono1 on nano s+/x). Verified on Speculos for **flex**
      (Gray4) and **nano s+** (Mono1).
- [x] a graphics test in the [`sadik`](../apps/sadik/) V-App (`DrawTest`, screen-size
      aware), passing on native, flex and nano s+.
- [ ] raw input events (`Touch` / `Button`) through `get_event`

## Open questions / risks

- **Speed.** A full-screen *`Canvas` + `flush()`* is slow on large screens: the
  framebuffer (~144 KB packed on Flex) lives in guest memory paged to the host 256
  bytes at a time, and the VM's data page cache (~12 pages) is far smaller than the
  frame, so it thrashes (~1000+ host round-trips). The fix, now implemented, is
  `render_banded` (`app-sdk/src/ux/canvas.rs`): it renders the scene band by band
  into one small, cache-resident buffer, so the framebuffer never round-trips to the
  host, and pairs with the draw/refresh split (one panel refresh per frame). The
  demo `draw` uses it; it runs with the default 64 KiB heap and is markedly faster
  on Speculos and hardware. Remaining levers for incremental updates: dirty-rectangle
  `flush_area` (avoid full redraws), and a larger data page cache. Worth measuring
  with [`bench/`](../bench).
- **NBGL low-level API**: `nbgl_frontDrawImage` / `nbgl_frontRefreshArea` are BOLOS
  syscalls whose C stubs link into `ledger_secure_sdk_sys` but are not exposed by
  its generated bindings, so the VM declares them itself via `extern "C"`. Verified
  on Speculos (Flex/Nano S+) and on a real Flex + Nano S+ device. The non-obvious
  constraints (column-major buffer, `y0`/`height` multiple of 4) are described in
  the handler section above — note they are *not* enforced by Speculos, so the
  emulator is not sufficient to validate this path. For 1bpp (`Mono1`), NBGL treats
  the `colorMap` as the *foreground* color and the area's `backgroundColor` as the
  bit-clear color, so the VM passes foreground = `WHITE`, background = `BLACK`, and
  uses `BLACK_AND_WHITE_REFRESH`. Open: confirm on stax / apex_p, and whether a
  refresh mode other than `FULL_COLOR_REFRESH` is preferable for partial updates.
- **Canvas memory**: storing one byte per pixel in guest RAM is simplest but costs
  ~268 KB for a Stax-sized canvas; packing at the native bit depth halves
  (`Gray4`) or eighths (`Mono1`) it at the cost of slightly more code in
  `set_pixel`.
