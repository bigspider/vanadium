// Re-export from the app SDK
pub use app_sdk::hash;

// The client↔V-App transport seam, transport-independent so it builds on wasm too.
pub mod transport_iface;
pub use transport_iface::{VAppExecutionError, VAppTransport};
#[cfg(feature = "wasm")]
pub use transport_iface::{GlobalDeviceTransport, WasmAppTransport};

// `elf` (loads V-App binaries, uses std::fs) and `memory` are only needed by the native
// VM engine, so they ride with the `transport` feature and stay out of wasm builds.
#[cfg(feature = "transport")]
pub mod elf;
#[cfg(feature = "transport")]
pub mod memory;

#[cfg(feature = "transport")]
mod apdu;
#[cfg(feature = "transport")]
pub mod comm;
#[cfg(feature = "transport")]
pub mod hmac_auth;
#[cfg(feature = "transport")]
pub mod linewriter;
#[cfg(feature = "transport")]
pub mod transport;
#[cfg(feature = "transport")]
pub mod transport_native_hid;
#[cfg(feature = "transport")]
pub mod vanadium_client;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use common::manifest;

// re-export if using the cargo_toml feature
#[cfg(feature = "cargo_toml")]
pub use cargo_toml;
