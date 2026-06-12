# Low-level graphics ECALLs

> Status: **experimental / proof-of-concept**. The blit ECALL, the accelerated
> command-stream ops, their pixel formats / colors, and the SDK `Canvas` / `Screen`
> abstractions are all subject to change. A design review of this surface against
> the goal of freezing the core ECALLs produced a concrete revision — see
> [Stabilization proposal: display ABI v2](#stabilization-proposal-display-abi-v2)
> at the end of this document.

> **Two drawing models.** There are now two ways to put pixels on the screen, with
> opposite trade-offs:
>
> 1. **Command stream (recommended, fast):** issue a handful of coarse draw ops
>    (`display_fill_rect`, `display_draw_text`, …) that the VM forwards to the device's
>    *native* NBGL drawing, operating on the framebuffer the OS already owns. Nothing is
>    rasterized in the guest and only tiny descriptors cross the ECALL boundary. SDK type:
>    [`ux::screen::Screen`](../app-sdk/src/ux/screen.rs). This is the path to prefer on
>    large screens.
> 2. **Blit (general, slow):** rasterize arbitrary pixels into a guest-RAM framebuffer
>    with `embedded-graphics` and push them with `display_blit`. SDK type:
>    [`ux::canvas::Canvas`](../app-sdk/src/ux/canvas.rs). Keep this for content the
>    command stream can't express (arbitrary computed pixels, full 16-level grayscale).
>
> The rest of this document first describes the blit model (the original design), then
> the command-stream model and why it is dramatically faster on real hardware.

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

> **Update — this principle does not hold on real hardware.** The "draw in the guest"
> reasoning assumed guest rasterization is free. It is not: the VM is a *software RISC-V
> interpreter* over paged, encrypted, Merkle-authenticated memory, so every rasterized
> pixel is interpreted, and a full framebuffer (~144 KB on Flex) thrashes the tiny data
> page cache. A full-screen blit is consequently very slow on Flex/Stax. `render_banded`
> (below) hides the paging cost but multiplies the *interpreted rasterization* cost (it
> re-runs the whole scene once per band). The per-primitive alternative was rejected only
> because of how chatty an `embedded-graphics` `DrawTarget` would be if it emitted one
> ECALL per pixel run — but that does **not** apply to *coarse, app-driven* ops called a
> handful of times per frame, which is exactly NBGL's own model. Those ops are now
> provided (see [Accelerated command-stream ops](#accelerated-command-stream-ops)) and
> are the recommended path; the blit stays as the fallback for arbitrary pixels.

## The blit ECALL

```rust
// common/src/ecall_constants.rs
pub const ECALL_DISPLAY_BLIT: u32 = 40;

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
| Apex P           | 300×400     | `Mono1`       |

(The Apex P panel is e-ink like Stax/Flex but 1bpp monochrome — see
`NATIVE_PIXEL_FORMAT` in [`vm/src/handlers/lib/ecall.rs`](../vm/src/handlers/lib/ecall.rs).)

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

This is now implemented: the VM decodes raw seph touch/button packets while pumping
events (`wait_for_ticker`), queues them (`EventQueue` in
[`ux_handler.rs`](../vm/src/handlers/lib/ecall/ux_handler.rs), depth 4, consecutive
touches coalesced), and `get_event` delivers them to the guest ahead of tickers from
its internal FIFO. The SDK surfaces them as `Event::Touch` / `Event::Button`.

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

## Accelerated command-stream ops

Instead of rasterizing in the guest and blitting, a V-App can issue **draw commands**
that the VM forwards to the device's native NBGL drawing, which operates on the
framebuffer the OS already owns and persists between calls. This removes both costs of
the blit model at once: there is **no guest framebuffer** to page, and **no per-pixel
rasterization** in the interpreter. Only a small descriptor (or a short string) crosses
the ECALL boundary per op.

This is exactly how NBGL itself is fast: its low-level primitives are coarse and
descriptor-shaped, glyphs/icons are native, and only changed regions are refreshed.

### The ECALLs

```rust
// common/src/ecall_constants.rs
pub const ECALL_DISPLAY_FILL_RECT: u32 = 42;  // -> nbgl_frontDrawRect
pub const ECALL_DISPLAY_DRAW_TEXT: u32 = 43;  // -> nbgl_drawText (OS fonts)

display_fill_rect(x, y, w, h, color) -> u32;
display_draw_text(x, y, w, h, text, text_len, color_font) -> u32;
```

- `color` is a [`Color`](../common/src/ecall_constants.rs) — NBGL's **4-color palette**
  (`Black`, `DarkGray`, `LightGray`, `White`). The vector primitives are 4-color; for
  full 16-level grayscale use the blit path.
- `color_font` packs `(bg << 16) | (color << 8) | font`: `color` is the text color, `bg`
  the color behind the text (NBGL fills the text box with it and anti-aliases the glyphs
  against it), and `font` a semantic [`Font`](../common/src/ecall_constants.rs)
  (`Regular` / `Bold` / `Large`) that the VM maps to the device's matching
  `nbgl_font_id_e` (the font sets differ per device).
- `display_fill_rect` does **not** require 4-row alignment: `nbgl_frontDrawRect` aligns
  `y0`/`height` itself and preserves the partial rows. (The blit path's column-major /
  4-row constraints do not apply here.)
- Like the blit ops, these only touch the framebuffer; call `display_refresh` once after
  a batch to push the result to the panel.

### Refresh modes

`display_refresh`'s last argument is now a [`RefreshMode`](../common/src/ecall_constants.rs)
(`FullColor` / `Partial` / `BlackWhite` / `BlackWhiteFast`) rather than a pixel format.
The panel refresh is the expensive part of an e-ink update, so picking a partial or fast
B&W mode for small or monochrome updates is a real performance lever. The SDK defaults to
`FullColor` on grayscale screens and `BlackWhite` on monochrome ones.

### Which NBGL functions are reachable

The VM can only call NBGL functions that are either BOLOS **syscalls** (stubbed in
`nbgl_stubs.S`) or compiled into the VM. `nbgl_frontDrawRect` (and the rest of the
`nbgl_front*` family, in `src/syscalls.c`) and `nbgl_drawText` (syscall) qualify.
`nbgl_drawRoundedRect`, `nbgl_drawQrCode` and `nbgl_drawIcon` live in `nbgl_draw.c`,
which the VM does **not** compile and which are not syscalls — so rounded rectangles, QR
codes and icons are **not yet available**. Adding them would require compiling
`nbgl_draw.c` into the VM (or new syscalls). Compressed-image ops (`nbgl_frontDrawImageRle`
/ `…File`) are reachable but need build-time asset tooling and are not wired up yet.

### SDK abstraction

V-Apps use [`ux::screen::Screen`](../app-sdk/src/ux/screen.rs):

```rust
use vanadium_app_sdk::ux::screen::{Screen, Color, Font};

let s = Screen::new();          // queries device geometry + native format
s.clear(Color::White);
s.fill_rect(0, 0, s.width(), 4, Color::Black);          // a top rule
s.draw_text(20, 20, s.width() - 40, 40, "Vanadium", Font::Large, Color::Black);
s.refresh();                    // single panel refresh
```

`Screen` is stateless (the OS holds the framebuffer); the `test` V-App's `draw` demo uses
it for large screens. **Speculos does not enforce the on-device NBGL constraints, so font
rendering and the color palette must be validated on real hardware.**

## Numbering & stabilization

Unlike `show_page` / `show_step` (which are genuinely Ledger-specific and live in
the vendor range 192–255), the display ops are generic and target-portable: they are
part of the **core** ECALL set the project intends to stabilize, and live in a
dedicated block (40–63, with room reserved for the planned icon / line / rounded-rect
/ QR / compressed-image ops — see the [v2 numbering](#numbering) section).

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
- Command-stream ops:
  - [x] `display_fill_rect` / `display_draw_text` end-to-end (constants, codegen, trait,
        riscv + native delegates, VM `fill_rect` / `draw_text` in `ux_handler.rs`).
  - [x] `display_refresh` made `RefreshMode`-aware.
  - [x] SDK abstraction: [`ux::screen::Screen`](../app-sdk/src/ux/screen.rs); the `test`
        V-App `draw` demo uses it on large screens. **Pending on-device validation**
        (font ids, palette, positioning) — Speculos does not enforce these.
  - [ ] rounded-rect / QR / icon ops (need `nbgl_draw.c` compiled into the VM or new
        syscalls) and compressed-image ops (need asset tooling).
- [x] raw input events (`Touch` / `Button`) through `get_event` (queued/coalesced in
      the VM's `EventQueue`; see the [Input](#input) section)

## Open questions / risks

- **Speed.** A full-screen *`Canvas` + `flush()`* is slow on large screens for two
  independent reasons. (1) *Paging:* the framebuffer (~144 KB packed on Flex) lives in
  guest memory paged to the host 256 bytes at a time, and the VM's data page cache
  (~12 pages) is far smaller than the frame, so it thrashes (~1000+ host round-trips).
  (2) *Interpreted rasterization:* every `embedded-graphics` pixel is rasterized in the
  software RISC-V interpreter. `render_banded` (`app-sdk/src/ux/canvas.rs`) fixes (1) by
  rendering band by band into one small, cache-resident buffer — but it makes (2) worse,
  because it re-runs the whole scene closure once per band (≈75 passes on Flex with 8-row
  bands), re-rasterizing each primitive's full bounding box every pass. So on real
  hardware the blit path remains slow even banded. The structural fix, now implemented,
  is the [command-stream ops](#accelerated-command-stream-ops): native drawing in the OS
  framebuffer eliminates *both* costs (no guest framebuffer, no interpreted
  rasterization). Use `Screen` for the common case and reserve the blit path for
  arbitrary computed pixels. Remaining blit-path levers if it must be used: larger /
  adaptive `render_banded` bands (fewer re-render passes), dirty-rectangle `flush_area`,
  and a larger data page cache. Worth measuring on a real device with [`bench/`](../bench).
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

---

# Stabilization proposal: display ABI v2

> Status: **proposal, not implemented**. This is the outcome of a design review of the
> v1 surface (everything above) against the requirement that the core ECALLs be generic
> and stable across future devices. Since nothing is frozen yet, v2 *replaces* v1
> wholesale in one breaking sweep — no compatibility aliases. Each change below states
> what it fixes.

## Design rules

The v1 surface has the right shape (blit + coarse accelerated ops, draw decoupled from
refresh), but several **current-hardware quirks leak into the contract**, and the error
and discoverability stories are inconsistent. v2 follows five rules:

1. **No device quirk in the contract.** Anything that varies per device — alignment
   granularity, palette, text limits, input model — is *queried* via device properties,
   never assumed. Today's NBGL constants appear only as example values.
2. **Parameter errors are never fatal.** A malformed display call returns an error code
   to the app; only guest memory-access violations abort the V-App. (v1 kills the app
   for a misaligned blit or a 513-byte string, while an out-of-bounds rect returns 0 —
   an arbitrary split.)
3. **Errors carry a reason.** Display ops return `i32`: `>= 0` is success, `< 0` a
   `DISPLAY_ERR_*` code. v1's bare `0` is undebuggable on device and collides with
   legitimate results (`display_text_width("")`).
4. **`0` is never a valid encoding for an enum the app passes in.** `PixelFormat`,
   `RefreshMode` and `Font` start at 1, so 0 uniformly means invalid/unknown — including
   in `get_device_property`, which returns 0 for unknown properties instead of aborting.
5. **Effects are defined, not inherited from NBGL.** Wherever v1's answer was "whatever
   this device's NBGL does" (non-native blit formats, gray on mono panels, text
   overflowing its box), v2 specifies the result.

Non-goal: command batching. The accelerated ops are coarse (a handful per frame), so one
ECALL per op is fine; revisit only if profiling on hardware says otherwise.

## Numbering

Display ops move to a dedicated block, **40–63**. v1 wedged them into 12–18 around the
pre-existing `get_device_property = 15`, leaving a single free slot — while rounded
rects, icons, lines, QR codes and compressed images are all planned. (The 20/21
collision with the storage ECALLs that briefly shipped on this branch is the other
argument: numbering needs room *and* a guard.)

```rust
pub const ECALL_DISPLAY_BLIT: u32 = 40;
pub const ECALL_DISPLAY_REFRESH: u32 = 41;
pub const ECALL_DISPLAY_FILL_RECT: u32 = 42;
pub const ECALL_DISPLAY_DRAW_TEXT: u32 = 43;
pub const ECALL_DISPLAY_TEXT_WIDTH: u32 = 44;
pub const ECALL_DISPLAY_FONT_METRICS: u32 = 45;
// 46..=63 reserved for future display ops (icon, line, rounded rect,
// QR code, compressed images, mode-switch if ever needed, ...).
```

Plus a compile-time uniqueness guard over all `ECALL_*` constants in `common` (a const
assertion or a unit test that collects and sorts them), so a collision can never again
compile silently.

## Common conventions

- **Coordinates are packed in pairs**, as `DEVICE_PROPERTY_SCREEN_SIZE` already does:
  `pos = (x << 16) | y` and `size = (w << 16) | h`, each component a `u16`. This frees
  registers for clean semantic arguments — the cramped 8-register ABI is the only reason
  v1 had to invent the `color_font` bitfield, whose documented layout had already
  drifted from the code within this branch.
- **Return type `i32`** (in `a0`). Negative values are errors:

```rust
pub const DISPLAY_ERR_INVALID_ARG: i32 = -1;   // malformed value: unknown enum, bad UTF-8, interior NUL, nonzero reserved bits
pub const DISPLAY_ERR_UNSUPPORTED: i32 = -2;   // well-formed but newer than this VM/device (probe-able)
pub const DISPLAY_ERR_OUT_OF_BOUNDS: i32 = -3; // rectangle not contained in the screen
pub const DISPLAY_ERR_BAD_LAYOUT: i32 = -4;    // stride / buffer_len inconsistent with the geometry
pub const DISPLAY_ERR_ALIGNMENT: i32 = -5;     // DEVICE_PROPERTY_DISPLAY_GRANULARITY violated
pub const DISPLAY_ERR_TOO_LONG: i32 = -6;      // text exceeds DEVICE_PROPERTY_MAX_TEXT_LEN
```

  `INVALID_ARG` vs `UNSUPPORTED` is the forward-compatibility hinge: an app probing a
  new `PixelFormat` or `Font` role on an older VM gets `UNSUPPORTED` and falls back,
  instead of being indistinguishable from a bug.

## Device properties

`get_device_property` changes contract: **an unknown property returns 0** (v1 aborts the
V-App), and every defined property has a nonzero encoding so 0 is unambiguous.

```rust
pub const DEVICE_PROPERTY_ID: u32 = 0x01;                  // unchanged
pub const DEVICE_PROPERTY_SCREEN_SIZE: u32 = 0x02;         // unchanged: (w << 16) | h
pub const DEVICE_PROPERTY_FEATURES: u32 = 0x03;            // bits now defined, below
pub const DEVICE_PROPERTY_PIXEL_FORMAT: u32 = 0x04;        // a PixelFormat (now >= 1)
pub const DEVICE_PROPERTY_DISPLAY_GRANULARITY: u32 = 0x05; // packed alignments, below
pub const DEVICE_PROPERTY_MAX_TEXT_LEN: u32 = 0x06;        // max text bytes per draw/measure op
pub const DEVICE_PROPERTY_ABI_REVISION: u32 = 0x07;        // >= 1, bumped on ABI additions
```

**`DEVICE_PROPERTY_FEATURES`** (v1 returns 0 with "to be defined"; meanwhile the SDK
hardcodes `native_text` / `partial_refresh` / `fast_mono_refresh` to `true` and infers
the input model from a device-ID table — `has_page_api()` — that *panics on unknown
devices*, which defeats the point of a device-independent VM):

```rust
pub const FEATURE_TOUCH: u32 = 1 << 0;             // absolute-pointer input; get_event may deliver Touch
pub const FEATURE_BUTTONS: u32 = 1 << 1;           // hardware buttons; get_event may deliver Button
pub const FEATURE_ACCEL_RECT: u32 = 1 << 2;        // display_fill_rect available
pub const FEATURE_ACCEL_TEXT: u32 = 1 << 3;        // display_draw_text / _text_width / _font_metrics available
pub const FEATURE_PARTIAL_REFRESH: u32 = 1 << 4;   // display_refresh honors sub-rectangles
pub const FEATURE_FAST_MONO_REFRESH: u32 = 1 << 5; // Mono/MonoFast refresh meaningfully cheaper than FullQuality
```

Current devices: all of `ACCEL_RECT | ACCEL_TEXT | PARTIAL_REFRESH | FAST_MONO_REFRESH`,
plus `TOUCH` on stax/flex/apex_p and `BUTTONS` on nanox/nanosplus. The SDK
`Capabilities` then reads these instead of hardcoding, and `InputModel` comes from the
TOUCH/BUTTONS bits — the `has_page_api()` table is deleted.

**`DEVICE_PROPERTY_DISPLAY_GRANULARITY`**: packed
`(x_align << 24) | (y_align << 16) | (w_align << 8) | h_align`, each a power of two
`>= 1`. Today's NBGL devices report `(1, 4, 1, 4)` = `0x01040104`. It constrains
`display_blit` destination rectangles (violation → `DISPLAY_ERR_ALIGNMENT`);
`display_refresh` self-aligns instead (see below); `display_fill_rect` and
`display_draw_text` have no caller-visible granularity. This replaces the hardcoded
"y and h must be multiples of 4" — an NBGL/e-ink driver quirk that v1 baked into the
generic contract (and that the SDK duplicates as `& !3` in `align4_clip` and
`Canvas::flush_area`). A future panel with byte-page rows (8), per-pixel freedom (1), or
column constraints just advertises different values and existing app binaries keep
working.

**`DEVICE_PROPERTY_MAX_TEXT_LEN`**: today 512 (`MAX_DISPLAY_TEXT_LEN`), which in v1 is
invisible — not in the trait docs, not queryable, unenforced on native/Speculos, *fatal*
on device.

**`DEVICE_PROPERTY_ABI_REVISION`**: coarse insurance for semantic changes that don't fit
a feature bit. Feature bits remain the primary probe; apps should not gate on the
revision unless a bit doesn't exist for what they need.

## Colors are RGB888

Every color argument becomes `0x00RRGGBB` (top byte reserved-zero; nonzero →
`DISPLAY_ERR_INVALID_ARG`, keeping the door open for alpha or wider gamuts).

v1's `Color` enum is documented as "NBGL's 4-color palette" with `color_t` values —
i.e. today's Ledger hardware baked into the supposedly stable ABI. A future color panel
could express nothing beyond 4 levels without changing every draw signature, and the
behavior of `DarkGray`/`LightGray` on the 1bpp Apex panel is *unspecified* (the SDK
theme code already dodges it empirically: "gray collapses to white").

In v2 the device renders the **nearest color representable by that operation's path**,
with a normative quantization for non-color panels so results are deterministic:

```text
luma = (77*R + 150*G + 29*B + 128) >> 8          # 0..=255 (BT.601 integer approximation)

Gray4 surface (blit-equivalent):  level = luma >> 4
4-level accelerated path (NBGL color_t): index = luma >> 6   # Black, DarkGray, LightGray, White
Mono1 surface:                    luma >= 128 ? white : black
```

The four canonical values `0x000000`, `0x555555`, `0xAAAAAA`, `0xFFFFFF` quantize
*exactly* to NBGL's palette on every current path (luma 0/85/170/255 → `color_t` 0/1/2/3
and Gray4 0/5/10/15, matching `EXPAND_TO_4BPP`), so the SDK keeps a `Color` type with
those named constants and app code barely changes. On Mono1, `0x555555` is defined to
render black and `0xAAAAAA` white — the previously unspecified case.

Conscious trade-off: quantization means a color request is *approximated, never
rejected*. Pixel-exact output remains the blit path's job, in the native `PixelFormat`.

## Pixel formats

```rust
#[repr(u32)]
pub enum PixelFormat {
    /// 1 bpp: 0 = black, 1 = white. Rows MSB-first, padded to a byte: stride = (w+7)/8.
    Mono1 = 1,
    /// 4 bpp grayscale: 0 = black ..= 15 = white. High nibble = left pixel,
    /// rows padded to a byte: stride = (w+1)/2.
    Gray4 = 2,
}
```

Two changes: encodings start at 1 (rule 4), and Mono1 is defined as **black/white**
rather than v1's "background/foreground", which implied a configurability that doesn't
exist (both the VM and the native backend already render 0=black, 1=white).

**A VM must accept every `PixelFormat` defined at its ABI revision, on every device**,
converting when the format isn't the panel's native one:

- `Mono1 → Gray4`: 0 → 0, 1 → 15.
- `Gray4 → Mono1`: level >= 8 → white, else black (fixed threshold, no dithering, so
  output is deterministic and testable).

Rationale: v1 forwards a non-native format straight to NBGL, which renders **gibberish**
(a Gray4 blit on the 1bpp panels — found the hard way with the SDK status icons, and
masked by Speculos). Silent garbage is the one behavior a stable ABI cannot have. Of
the two fixes, conversion is chosen over rejection because it makes blits
device-independent — Mono1 art becomes a universal donor format, and today's binaries
keep rendering on tomorrow's panels — and it is effectively free: the VM's blit path
already visits every pixel in the band transpose. A format *newer than the VM* fails
soft with `DISPLAY_ERR_UNSUPPORTED`, so apps can probe and fall back. Apps should still
prefer the advertised native format: conversion preserves correctness, not fidelity.

## The ops

### display_blit

```rust
display_blit(
    dst: u32,           // (x << 16) | y — destination top-left, screen px
    size: u32,          // (w << 16) | h
    buffer: *const u8,  // a source bitmap in guest memory, row-major, top-left origin
    buffer_len: u32,    // total readable bytes at `buffer`
    src: u32,           // (x << 16) | y — top-left of the source rect inside the bitmap
    src_stride: u32,    // bytes between consecutive bitmap rows
    format: u32,        // PixelFormat of the bitmap
) -> i32                // 0 = success, DISPLAY_ERR_* < 0
```

This is the classic copy-rect: a rectangle *inside a larger source bitmap*, instead of
v1's exactly-packed buffer. Rationale: the dominant real call is "flush a dirty
sub-rectangle of a full-frame guest framebuffer", and under v1 the SDK must repack that
sub-rect row by row into a temporary allocation (the "general path" in
`Canvas::flush_area`) because its rows aren't contiguous and its left edge isn't
byte-aligned. With `src`/`src_stride` the VM reads each band row at the right guest
offset directly — and arbitrary bit offsets cost nothing, because the band transpose
already addresses every pixel individually. v1 is the degenerate case
`src = 0, src_stride = stride(format, w)`. Adding this later would mean a second blit
ECALL; adding it now is free.

Validation, in order (all soft):

1. `format` known → else `INVALID_ARG` (malformed) / `UNSUPPORTED` (newer than VM);
2. `w == 0 || h == 0` → success, no-op;
3. destination rect within the screen → else `OUT_OF_BOUNDS`;
4. destination rect meets `DEVICE_PROPERTY_DISPLAY_GRANULARITY` → else `ALIGNMENT`;
5. every byte the source rect addresses lies within `buffer_len`; for the row-major
   layout this is
   `(src_y + h - 1) * src_stride + ceil(((src_x + w) * bpp) / 8) <= buffer_len`
   → else `BAD_LAYOUT`.

Draws to the framebuffer only; `display_refresh` makes it visible (the two-phase model
is unchanged — it is the part of v1 that is right).

### display_refresh

```rust
display_refresh(
    pos: u32,   // (x << 16) | y
    size: u32,  // (w << 16) | h
    mode: u32,  // RefreshMode (a hint)
) -> i32

#[repr(u32)]
pub enum RefreshMode {
    /// Best quality the panel offers. (v1 `FullColor` — renamed: nothing about it is color.)
    FullQuality = 1,
    /// Localized update, quality maintained. (v1 `Partial`.)
    Partial = 2,
    /// Black & white, contrast priority. (v1 `BlackWhite`.)
    Mono = 3,
    /// Black & white, speed priority. (v1 `BlackWhiteFast`.)
    MonoFast = 4,
}
```

Two semantic changes:

- **Modes are advisory.** The device maps a requested mode to the nearest thing its
  panel supports: a mono panel treats `FullQuality` as `Mono`; a fast LCD may ignore
  modes entirely. Every *defined* mode therefore succeeds on every device; only a mode
  value newer than the VM returns `UNSUPPORTED` (probe-able). This legitimizes what the
  hardware already forces and lets future panels map the vocabulary sensibly.
- **The VM aligns the rectangle itself**, expanding outward to the granularity and
  clipping to the screen. Unlike a blit — where expansion would require pixels the
  caller didn't provide — refreshing a slightly larger area is harmless: the framebuffer
  already holds the correct pixels. This removes v1's inconsistency (blit enforced
  alignment *fatally*; refresh didn't enforce it at all and fed unaligned areas to the
  driver with unverified results) and deletes the SDK-side `align4_clip` duplication.

### display_fill_rect

```rust
display_fill_rect(
    pos: u32,   // (x << 16) | y
    size: u32,  // (w << 16) | h
    color: u32, // RGB888
) -> i32
```

Semantics unchanged from v1 apart from the RGB color and packed coordinates. No
granularity constraint: the implementation must handle partial rows internally (as
NBGL's `nbgl_frontDrawRect` already does).

### display_draw_text

```rust
display_draw_text(
    pos: u32,          // (x << 16) | y — top-left of the text box
    size: u32,         // (w << 16) | h — the clip box
    text: *const u8,   // UTF-8, no interior NUL
    text_len: u32,     // bytes; <= DEVICE_PROPERTY_MAX_TEXT_LEN
    font: u32,         // Font role
    color: u32,        // RGB888 text color
    bg: u32,           // RGB888 box fill / anti-alias background
) -> i32
```

Font, color and bg are separate registers (affordable thanks to coordinate packing),
deleting the `color_font` bitfield.

**Defined rendering semantics** (v1 inherited "whatever NBGL does on this device" for
all of these):

1. The box is filled with `bg`, which is also the anti-aliasing background.
2. The string is drawn as one line, the font's top edge at the box top, left edge at
   the box left. (Alignment/centering is the caller's job via measurement — as the SDK
   already does.)
3. **Glyphs are clipped to the box**: text wider than `w` or taller than `h` never
   paints outside it. On NBGL this may require the VM to truncate at the last fitting
   glyph using `nbgl_getTextWidth` prefix measurement — the contract is the clip; the
   technique is the VM's business.
4. Both colors quantize per the RGB rules; anti-aliasing on grayscale panels blends
   between the quantized fg and bg.

Errors: bad UTF-8 / interior NUL / nonzero top color byte → `INVALID_ARG`; font role
newer than the VM → `UNSUPPORTED` (v1 inconsistently rejected a bad fg but silently
replaced a bad bg with White); box off-screen → `OUT_OF_BOUNDS`; over the advertised
length → `TOO_LONG` (soft — in v1 this kills the V-App, and only on real hardware).

### display_text_width

```rust
display_text_width(font: u32, text: *const u8, text_len: u32) -> i32
// >= 0: rendered width in px (must equal what display_draw_text would render)
// < 0:  DISPLAY_ERR_*
```

Same text validation as `display_draw_text`. The signed convention removes v1's
ambiguity where `0` meant both "error" and "empty string".

### display_font_metrics

```rust
display_font_metrics(font: u32) -> i32
// >= 0: (height << 16) | line_height, both in px
// < 0:  DISPLAY_ERR_*

#[repr(u32)]
pub enum Font {
    Regular = 1,
    Bold = 2,
    Large = 3,
}
```

Heights are `<= 0x7FFF`, so the packed value never enters the error space. Semantic
roles rather than font ids stay — that part of v1 is right: apps lay out against
queried metrics, so per-device font differences can't break them. New roles may be
appended; an old VM answers `UNSUPPORTED` and the app falls back. If richer metrics
(baseline/ascent/descent for mixed-font lines) become necessary, they are a new ECALL
in the reserved block, not a repacking.

## Input events

The 16-byte `EventData` payload and the `get_event` ABI stay. One wording change buys
all future extensibility:

> Bytes of a defined event's payload beyond its declared fields are **reserved and must
> be zero**.

v1 says they are "undefined and could change in future versions" — which would make it
forever impossible to add a field, since an old VM could legitimately emit garbage where
the new field lives. The VM already zeroes the payload (`EventData::default()` before
writing the variant), so this documents reality and keeps e.g. a millisecond timestamp
(gesture velocity) or a touch contact id (multi-touch) addable without a new event code.

Payload layouts (zero-invalid states per rule 4):

```rust
TouchEvent  { x: u16, y: u16, state: u8 /* 1 = Pressed, 2 = Released */, _reserved: u8 }
ButtonEvent { button: u8 /* 1 = Left, 2 = Right, 3 = Both */, state: u8 /* 1 = Pressed, 2 = Released */ }
```

`ButtonEvent` becomes (button, state) instead of v1's six-variant enum: the same
information, but a future device with more buttons adds *ids* rather than a
combinatorial set of variants. "Both" stays a distinct id because the OS itself
synthesizes it as a gesture.

Queueing contract (the VM's `EventQueue`):

- Discrete events (buttons, future kinds) are delivered **in order and none is
  silently lost** within a queue window; on overflow (depth 4) the *oldest* event is
  dropped — documented behavior, not an implementation accident.
- Touch coalescing merges **Pressed onto Pressed only** (a drag's move-flood). It never
  merges across a press/release edge: v1 coalesces *any* touch onto *any* queued touch,
  so a fast tap completed within one ticker window delivers only the `Released` and the
  press edge is lost — contradicting the framework's own "react on press" goal.
- If input arrived while `get_event` was pumping for a ticker, the input is returned
  **before** the ticker. (v1 returns the ticker first and the input on the next call —
  mostly harmless, but ordering must be defined, and input-first is the useful
  definition.)

## Screen ownership

Stated contract (no new ECALL needed yet):

- The first `display_*` call puts the V-App in **direct-draw mode**: the framebuffer
  belongs to the app, the VM draws nothing of its own, and input arrives as raw
  Touch/Button events.
- Any `show_page` / `show_step` ends direct-draw mode: NBGL repaints, framebuffer
  contents become **undefined**, and the next `display_*` call re-enters direct-draw
  mode where the app must repaint everything it relies on (a full repaint, not an
  incremental diff).
- On V-App exit the VM restores its own UI unconditionally.

This makes interleaving *defined* instead of forbidden-but-unchecked. An explicit
mode-switch ECALL still fits in the reserved block if a future device needs real
setup/teardown.

## The native backend is the reference implementation

Neither Speculos nor the v1 native backend enforces any of the above — both are *laxer*
than hardware (no granularity check, no length cap, any format accepted, geometry
hardcoded to a 400×672 Gray4 Stax regardless of target), so today the only place an app
discovers a contract violation is a physical device. v2 inverts that: **the native
backend is the strictest implementation.**

- Every validation above (granularity, `BAD_LAYOUT`, `TOO_LONG`, UTF-8, formats,
  reserved bits) enforced exactly as specified.
- Device profile selectable (e.g. `VAPP_DEVICE=flex|stax|apex_p|nanosplus|nanox`):
  geometry, native pixel format, granularity, features and input model match the
  target instead of always-Stax.
- Quantization and format conversion implemented with the normative formulas, so the
  PPM/simulator output is the reference rendering (modulo font shapes).

## Migration checklist

- [ ] `common`: renumber ECALLs and enums, error codes, new properties, feature bits
      (one breaking sweep), plus the ECALL-number uniqueness guard
- [ ] `vm`: `i32` status returns; all parameter errors soft; granularity/limits served
      from per-device constants via the new properties; `get_device_property` soft-fail
- [ ] `vm`: blit `src`/`src_stride` addressing; Gray4↔Mono1 conversion in `blit_band`
- [ ] `vm`: RGB quantization for fill/text; self-aligning refresh; text clipping
- [ ] `vm`: event-queue coalescing fix (Pressed-onto-Pressed only); input-before-ticker
- [ ] `app-sdk`: trait + riscv/native delegates; `Capabilities` from `FEATURES`
      (delete the `has_page_api()` device table); `Color` named constants over RGB;
      delete `align4_clip` / `flush_area` alignment duplication
- [ ] native backend: strict validation + device profiles (`VAPP_DEVICE`)
- [ ] `apps/test` + `sadik`: exercise every error code, the conversion paths, and the
      granularity property on all profiles
- [ ] docs: fold this section into the main text once implemented; update
      [`docs/ecalls.md`](./ecalls.md) with the error/property conventions
