#![cfg_attr(feature = "target_vanadium_ledger", no_std)]

extern crate alloc;

pub mod bip32;
pub mod constants;
pub mod handlers;
pub mod identity;
pub mod resident_key;

// Web entry (architecture A): the real BitcoinClient exposed to JS via wasm-bindgen.
#[cfg(feature = "target_wasm")]
pub mod wasm;

use handlers::*;

use alloc::vec::Vec;

use common::message::{Request, Response};
use sdk::{App, AppBuilder};

/// The app's identity + handler, shared by the native/riscv binary (`main.rs`) and the wasm
/// web entry (`wasm.rs`) so the name/description/developer live in exactly one place.
pub fn app_builder() -> AppBuilder {
    AppBuilder::new("Bitcoin", env!("CARGO_PKG_VERSION"), process_message)
        .description("Bitcoin is ready")
        .developer("Salvatore Ingala")
}

pub async fn handle_request(
    app: &mut App,
    request: &Request,
) -> Result<Response, common::errors::Error> {
    match request {
        Request::GetVersion => todo!(),
        Request::Exit => sdk::exit(0),
        Request::GetMasterFingerprint { tree } => handle_get_master_fingerprint(app, *tree),
        Request::GetExtendedPubkey {
            tree,
            path,
            display,
            identity_index,
        } => handle_get_extended_pubkey(app, *tree, path, *display, *identity_index).await,
        Request::RegisterAccount {
            name,
            account,
            registered_identities,
            key_signatures,
            show_cleartext,
        } => {
            handle_register_account(
                app,
                name,
                account,
                registered_identities.as_deref(),
                key_signatures.as_deref(),
                *show_cleartext,
            )
            .await
        }
        Request::RegisterIdentityKey { name, pubkey } => {
            handle_register_identity_key(app, name, pubkey).await
        }
        Request::GetAddress {
            name,
            account,
            por,
            coordinates,
            display,
            identity_index,
        } => {
            handle_get_address(
                app,
                name.as_deref(),
                account,
                por,
                coordinates,
                *display,
                *identity_index,
            )
            .await
        }
        Request::SignPsbt { psbt } => handle_sign_psbt(app, psbt).await,
    }
}

#[sdk::handler]
pub async fn process_message(app: &mut App, request: &[u8]) -> Vec<u8> {
    let mut decoder = minicbor::Decoder::new(request);
    let Ok(decoded_request) = decoder.decode::<Request>() else {
        return minicbor::to_vec(&Response::Error {
            error: common::errors::Error::InvalidRequest,
        })
        .unwrap();
    };
    if decoder.position() != request.len() {
        return minicbor::to_vec(&Response::Error {
            error: common::errors::Error::InvalidRequest,
        })
        .unwrap();
    }
    let response = handle_request(app, &decoded_request)
        .await
        .unwrap_or_else(|error| Response::Error { error });
    minicbor::to_vec(&response).unwrap()
}

