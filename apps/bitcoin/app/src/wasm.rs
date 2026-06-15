//! The Bitcoin V-App's web entry (architecture A).
//!
//! It installs the app into the page's **global device** and exposes the **real**
//! `BitcoinClient` to JS through wasm-bindgen — so each command is a JS method (`await
//! app.getExtendedPubkey(...)`). The generic device shell (framebuffer, input, dashboard /
//! idle pump) comes from the SDK (`sdk::wasm_runtime`); the only app-specific code here is
//! the per-command bindings, which are thin wrappers over the client's own methods.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::ToString;
use core::cell::RefCell;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use sdk::wasm_runtime::{self, CommandGuard};

use vnd_bitcoin_client::message::KeyTree;
use vnd_bitcoin_client::{BitcoinClient, GlobalDeviceTransport};

/// The Bitcoin app, callable from JS. Construct it once (this installs the V-App into the
/// page's device); then call its command methods, each of which runs end to end against the
/// co-resident app — suspending for on-device confirmation as needed, and resolving as a
/// Promise.
#[wasm_bindgen]
pub struct BitcoinApp {
    client: Rc<RefCell<BitcoinClient>>,
}

#[wasm_bindgen]
impl BitcoinApp {
    /// Installs the Bitcoin V-App into the page's global device and builds the client over it.
    #[wasm_bindgen(constructor)]
    pub fn new() -> BitcoinApp {
        wasm_runtime::install(crate::app_builder().build_wasm());
        BitcoinApp {
            client: Rc::new(RefCell::new(BitcoinClient::new(Box::new(
                GlobalDeviceTransport,
            )))),
        }
    }

    /// `GetMasterFingerprint` — request/response, no UI.
    #[wasm_bindgen(js_name = getMasterFingerprint)]
    pub fn get_master_fingerprint(&self) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let fp = client
                .borrow_mut()
                .get_master_fingerprint(KeyTree::Standard)
                .await
                .map_err(to_js)?;
            Ok(JsValue::from(fp))
        })
    }

    /// `GetExtendedPubkey` at `path` (e.g. `"m/84'/1'/0'"`). With `display`, the app shows an
    /// on-device confirmation and awaits a tap before the Promise resolves to the `xpub`.
    #[wasm_bindgen(js_name = getExtendedPubkey)]
    pub fn get_extended_pubkey(&self, path: String, display: bool) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let (xpub, _sig) = client
                .borrow_mut()
                .get_extended_pubkey(KeyTree::Standard, &path, display, None)
                .await
                .map_err(to_js)?;
            Ok(JsValue::from(bitcoin::base58::encode_check(&xpub)))
        })
    }
}

fn to_js(e: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&e.to_string()).into()
}
