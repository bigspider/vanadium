//! The Bitcoin V-App's web entry (architecture A).
//!
//! It installs the app into the page's **global device** and exposes the **real**
//! `BitcoinClient` to JS through wasm-bindgen — so each command is a JS method (`await
//! app.getAddress(...)`). The generic device shell (framebuffer, input, dashboard / idle
//! pump) comes from the SDK (`sdk::wasm_runtime`); the only app-specific code here is the
//! per-command bindings, which are thin wrappers over the client's own methods.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use base64::Engine as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use sdk::wasm_runtime::{self, CommandGuard};

use vnd_bitcoin_client::bip388::{KeyInformation, WalletPolicy};
use vnd_bitcoin_client::message::{Account, AccountCoordinates, KeyTree, WalletPolicyCoordinates};
use vnd_bitcoin_client::{BitcoinClient, GlobalDeviceTransport, ProofOfRegistration};

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

    /// `GetMasterFingerprint` — request/response, no UI. Resolves to the fingerprint (u32).
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
    /// on-device confirmation and awaits a tap. Resolves to the `xpub`/`tpub` string.
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

    /// `GetExtendedPubkey` for the i-th identity key (`m/1229210958'/i`). Pass a negative
    /// `index` for the unindexed key. With `display`, awaits on-device confirmation. Resolves
    /// to the identity `xpub`.
    #[wasm_bindgen(js_name = getIdentityKey)]
    pub fn get_identity_key(&self, index: i32, display: bool) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let idx = if index < 0 { None } else { Some(index as u32) };
            let xpub = client
                .borrow_mut()
                .get_identity_key(idx, display)
                .await
                .map_err(to_js)?;
            Ok(JsValue::from(bitcoin::base58::encode_check(&xpub)))
        })
    }

    /// `RegisterIdentityKey`: registers a 33-byte compressed pubkey (hex) under `name`. Shows
    /// an on-device review. Resolves to `{"id": <hex>, "hmac": <hex>}` (the proof of
    /// registration).
    #[wasm_bindgen(js_name = registerIdentityKey)]
    pub fn register_identity_key(&self, name: String, pubkey_hex: String) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let bytes = hex::decode(pubkey_hex.trim()).map_err(|_| to_js("invalid pubkey hex"))?;
            let arr: [u8; 33] = bytes
                .try_into()
                .map_err(|_| to_js("pubkey must be 33 bytes"))?;
            let (id, hmac) = client
                .borrow_mut()
                .register_identity_key(&name, &arr)
                .await
                .map_err(to_js)?;
            Ok(id_hmac_json(id.as_bytes(), &hmac.dangerous_as_bytes()))
        })
    }

    /// `RegisterAccount`: registers a BIP-388 wallet policy (`descriptor_template` + newline/
    /// comma-separated `keys_info`) under `name`. Shows an on-device review. Resolves to
    /// `{"id": <hex>, "hmac": <hex>}`; keep the `hmac` to pass as `por` to `getAddress`.
    #[wasm_bindgen(js_name = registerAccount)]
    pub fn register_account(
        &self,
        name: String,
        descriptor_template: String,
        keys_info: String,
        show_cleartext: bool,
    ) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let account = Account::WalletPolicy(parse_wallet_policy(&descriptor_template, &keys_info)?);
            let (id, hmac) = client
                .borrow_mut()
                .register_account(&name, &account, None, None, show_cleartext)
                .await
                .map_err(to_js)?;
            Ok(id_hmac_json(id.as_bytes(), &hmac.dangerous_as_bytes()))
        })
    }

    /// `GetAddress` for a wallet policy at `(is_change, address_index)`. `por_hex` is the
    /// proof of registration from `registerAccount` (empty for a default policy). With
    /// `display`, awaits on-device confirmation. Resolves to the address string.
    #[wasm_bindgen(js_name = getAddress)]
    #[allow(clippy::too_many_arguments)]
    pub fn get_address(
        &self,
        descriptor_template: String,
        keys_info: String,
        name: String,
        is_change: bool,
        address_index: u32,
        por_hex: String,
        display: bool,
    ) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let account = Account::WalletPolicy(parse_wallet_policy(&descriptor_template, &keys_info)?);
            let coords = AccountCoordinates::WalletPolicy(WalletPolicyCoordinates {
                is_change,
                address_index,
            });
            let por = if por_hex.trim().is_empty() {
                None
            } else {
                let bytes = hex::decode(por_hex.trim()).map_err(|_| to_js("invalid por hex"))?;
                let arr: [u8; 32] = bytes.try_into().map_err(|_| to_js("por must be 32 bytes"))?;
                Some(ProofOfRegistration::from_bytes(arr))
            };
            let (addr, _sig) = client
                .borrow_mut()
                .get_address(&account, &name, &coords, por.as_ref(), display, None)
                .await
                .map_err(to_js)?;
            Ok(JsValue::from(addr))
        })
    }

    /// `SignPsbt`: signs a base64 PSBTv0 with the given wallet policy. `name`/`por_hex` are the
    /// account name and proof of registration (from `registerAccount`) that authorize the
    /// policy. The PSBT is prepared with the policy's BIP-388 derivation info (as a real host
    /// would) before signing. Shows an on-device review. Resolves to
    /// `{"signatures": [{"inputIndex", "pubkey", "signature"}], "musigPubnonces", "musigPartialSigs"}`.
    #[wasm_bindgen(js_name = signPsbt)]
    #[allow(clippy::too_many_arguments)]
    pub fn sign_psbt(
        &self,
        psbt_base64: String,
        descriptor_template: String,
        keys_info: String,
        name: String,
        por_hex: String,
    ) -> js_sys::Promise {
        let client = self.client.clone();
        future_to_promise(async move {
            let _g = CommandGuard::new();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(psbt_base64.trim())
                .map_err(|_| to_js("invalid base64 PSBT"))?;
            let mut psbt = bitcoin::psbt::Psbt::deserialize(&raw)
                .map_err(|e| to_js(format!("invalid PSBTv0: {e}")))?;
            let wp = parse_wallet_policy(&descriptor_template, &keys_info)?;
            let por: [u8; 32] = hex::decode(por_hex.trim())
                .map_err(|_| to_js("invalid por hex"))?
                .try_into()
                .map_err(|_| to_js("por must be 32 bytes"))?;
            // Inject the policy's BIP-388 key-derivation info so the device recognizes its
            // inputs (what a wallet/host does before handing a PSBT to the signer).
            common::psbt::prepare_psbt(&mut psbt, &[(&wp, name.as_str(), &por)])
                .map_err(|e| to_js(format!("prepare_psbt: {e:?}")))?;
            let v2 = vnd_bitcoin_client::psbt_v0_to_v2(&psbt.serialize())
                .map_err(|_| to_js("failed to convert PSBT to v2"))?;
            let signed = client.borrow_mut().sign_psbt(&v2).await.map_err(to_js)?;
            let sigs: Vec<String> = signed
                .signatures
                .iter()
                .map(|p| {
                    format!(
                        "{{\"inputIndex\":{},\"pubkey\":\"{}\",\"signature\":\"{}\"}}",
                        p.input_index,
                        hex::encode(&p.pubkey),
                        hex::encode(&p.signature)
                    )
                })
                .collect();
            Ok(JsValue::from(format!(
                "{{\"signatures\":[{}],\"musigPubnonces\":{},\"musigPartialSigs\":{}}}",
                sigs.join(","),
                signed.musig_pubnonces.len(),
                signed.musig_partial_sigs.len()
            )))
        })
    }
}

fn parse_keys_info(keys_info: &str) -> Result<Vec<KeyInformation>, JsValue> {
    keys_info
        .split(['\n', ','])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| KeyInformation::try_from(s).map_err(|e| to_js(format!("invalid key info: {e:?}"))))
        .collect()
}

fn parse_wallet_policy(template: &str, keys_info: &str) -> Result<WalletPolicy, JsValue> {
    let keys = parse_keys_info(keys_info)?;
    WalletPolicy::new(template, keys).map_err(|e| to_js(format!("invalid wallet policy: {e:?}")))
}

fn id_hmac_json(id: &[u8], hmac: &[u8]) -> JsValue {
    JsValue::from(format!(
        "{{\"id\":\"{}\",\"hmac\":\"{}\"}}",
        hex::encode(id),
        hex::encode(hmac)
    ))
}

fn to_js(e: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&e.to_string()).into()
}
