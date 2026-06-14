//! The real Bitcoin V-App running in a web page: its `process_message` handler runs in
//! wasm, driven by a co-resident client that speaks the real Bitcoin CBOR protocol over
//! client-sdk's `WasmAppTransport` — no server, no device.
//!
//! Hand-rolled ABI (no wasm-bindgen): `bitcoin_init()` builds the transport once;
//! `bitcoin_get_fingerprint()` runs the GetMasterFingerprint command end-to-end and
//! returns the fingerprint.

extern crate alloc;

use std::cell::RefCell;

use sdk::executor::block_on;
use sdk::AppBuilder;

use client::{VAppTransport, WasmAppTransport};

use btc_common::message::{KeyTree, Request, Response};

thread_local! {
    static TRANSPORT: RefCell<Option<WasmAppTransport>> = const { RefCell::new(None) };
}

#[no_mangle]
pub extern "C" fn bitcoin_init() {
    TRANSPORT.with(|t| {
        *t.borrow_mut() = Some(WasmAppTransport::new(AppBuilder::new(
            "Bitcoin",
            env!("CARGO_PKG_VERSION"),
            vnd_bitcoin::process_message,
        )));
    });
}

/// The client: encode a `GetMasterFingerprint` request, send it to the co-resident Bitcoin
/// app over the transport, decode the response, and return the fingerprint (0 on error).
/// Pure request/response (no UI), so it completes in one drive.
#[no_mangle]
pub extern "C" fn bitcoin_get_fingerprint() -> u32 {
    let req = Request::GetMasterFingerprint {
        tree: KeyTree::Standard,
    };
    let req_bytes = minicbor::to_vec(&req).expect("encode request");
    let resp_bytes = TRANSPORT
        .with(|t| {
            let mut guard = t.borrow_mut();
            let transport = guard.as_mut().expect("bitcoin_init must be called first");
            block_on(transport.send_message(&req_bytes))
        })
        .expect("send_message failed");
    match minicbor::decode::<Response>(&resp_bytes) {
        Ok(Response::MasterFingerprint { fingerprint }) => fingerprint,
        _ => 0,
    }
}
