ECALLs allow Risc-V code to call system services, that are provided by the environment. The Vanadium VM defines ECALLs for low level primitives (communication, screen management, etc.) and access to the implementation cryptographic accelerator, or other functionalities the VM provides for performance reasons.

# Risc-V calling conventions for ECALLs

ECALLs use the following calling convention:

- ECALL code in `t0`
- Up to 8 ECALL arguments in `a0`, `a1`, ..., `a7`, in this order.
- Return value (if any) is in `a0`.

No ECALLs with more than 8 argments (using the stack) are currently defined.

# Currently defined ECALLs

See [ecalls.rs](../app-sdk/src/ecalls.rs) for the interface and documentation of the currently defined ECALLs.

# ABI conventions

The display ECALLs (the dedicated block 40–63; see [graphics.md](./graphics.md)) follow shared conventions, which new ECALLs should adopt:

- **`i32` status returns.** `>= 0` is success (the value's meaning is per-ECALL: 0 for drawing ops, a width or packed metrics for query ops); `< 0` is one of the `DISPLAY_ERR_*` codes in [`ecall_constants.rs`](../common/src/ecall_constants.rs). **Parameter errors are soft** — they are reported in the return value and never abort the V-App; only guest memory-access violations are fatal.
- **Zero-invalid enums.** In every ABI enum (`PixelFormat`, `Font`, `RefreshMode`, press states, …) the encoding 0 is reserved as invalid. An enum argument of 0 fails with `DISPLAY_ERR_INVALID_ARG` (malformed); any other unknown value fails with `DISPLAY_ERR_UNSUPPORTED`, because it may be a valid encoding on a newer VM — so an app can *probe* for a feature and fall back when it gets `UNSUPPORTED`.
- **Packed 16-bit pairs.** Positions and sizes are packed as `(x << 16) | y` / `(w << 16) | h` (`display_pack_pair` / `display_unpack_pair` in `common`), keeping argument counts within the 8 registers.
- **Device properties** (`get_device_property`): querying a property the VM does not know returns 0 — never an error — and every defined property has a nonzero value, so 0 unambiguously means "not supported here" and apps can probe properties added in later ABI revisions. Apps branch on `DEVICE_PROPERTY_FEATURES` bits, never on the device id.
- **Reserved space must be zero**: unused bytes of an event payload, the top byte of RGB888 colors, etc. — so fields can be added later without new codes (a nonzero reserved field on an old VM would be indistinguishable from garbage).

# Implementation of ECALLs

Each new ECALL requires:
- adding the appropriate constants in [`common/src/ecall_constants.rs`](../common/src/ecall_constants.rs);
- add the ECALL to [`app-sdk/src/ecalls.rs`](../app-sdk/src/ecalls.rs);
- implementing the ECALL for native compilation in [`app-sdk/src/ecalls_native.rs`](../app-sdk/src/ecalls_native.rs);
- implementing the ECALL code generation via the macros in [`app-sdk/src/ecalls_riscv.rs`](../app-sdk/src/ecalls_riscv.rs) and [`ecalls/src/lib.rs`](../ecalls/src/lib.rs);
- implementing the ECALL handler in the Vanadium VM in [`vm/src/handlers/lib/ecall.rs`](../vm/src/handlers/lib/ecall.rs);
- expose the functionality of the ECALL via the appropriate abstraction in the app-sdk;
- add code to the [sadik V-App](../apps/sadik/) in order to test the new ECALLs.

ECALLs are not exported directly in the `vanadium-app-sdk`. Rather, clean Rust abstractions are implemented. Apart from providing a cleaner interface, the goal of the abstraction is to avoid that the application code depends on the low-level details of ECALLs. This allows breaking changes in the ECALLs, or even target-specific ECALLs, without impacting the users of the crate.

Eventually, the goal is to stabilize a set of ECALLs that constitutes the core of Vanadium, in order to simplify adding new targets.
