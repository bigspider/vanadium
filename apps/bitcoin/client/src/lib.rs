extern crate bitcoin;

mod client;

pub use client::BitcoinClient;

// Re-export from the sdk. The native CLI helpers (`client_utils`, HID/TCP transports) live
// behind client-sdk's `transport` feature; on wasm only the transport-independent trait and
// the co-resident `WasmAppTransport` are available.
#[cfg(not(feature = "wasm"))]
pub use sdk::vanadium_client::{client_utils::*, VAppTransport};
#[cfg(feature = "wasm")]
pub use sdk::{VAppTransport, WasmAppTransport, WasmClientDriver};

// Re-exports from the `common` module that are useful for users of this library.
pub use common::{
    bip388::{self, WalletPolicy},
    identity::{self, IdentityKey},
    message::{self, IdentitySignature, RegisteredIdentityEntry},
    por::{ProofOfRegistration, RegistrationId},
    psbt::psbt_v0_to_v2,
};
