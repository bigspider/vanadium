//! The Bitcoin V-App's web entry (architecture A).
//!
//! It installs the app into the page's **global device** and exposes the **real**
//! `BitcoinClient` to JS through wasm-bindgen — so each command is a typed JS method (`const
//! addr: string = await app.getAddress(...)`). The generic device shell (framebuffer, input,
//! dashboard / idle pump) comes from the SDK (`sdk::wasm_runtime`); the only app-specific
//! code here is the per-command bindings, which are thin wrappers over the client's methods.
//!
//! Errors reject the Promise with a JS `Error` whose `name` distinguishes an on-device
//! rejection (`"UserRejected"`) from a V-App error (`"AppError"`) or any other failure.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use base64::Engine as _;
use wasm_bindgen::prelude::*;

use sdk::wasm_runtime::{self, CommandGuard};

use vnd_bitcoin_client::bip388::{KeyInformation, WalletPolicy};
use vnd_bitcoin_client::message::{Account, AccountCoordinates, KeyTree, WalletPolicyCoordinates};
use vnd_bitcoin_client::{BitcoinClient, BitcoinClientError, GlobalDeviceTransport, ProofOfRegistration};

/// The Bitcoin app, callable from JS. Construct it once (this installs the V-App into the
/// page's device); then `await` its command methods, each of which runs end to end against
/// the co-resident app — suspending for on-device confirmation as needed.
#[wasm_bindgen]
pub struct BitcoinApp {
    client: Rc<RefCell<BitcoinClient>>,
}

/// The result of registering an account or identity key: the object's id and its proof of
/// registration (`hmac`), both hex. Keep `hmac` to pass as the `por` to `getAddress`/`signPsbt`.
#[wasm_bindgen]
pub struct Registration {
    id: String,
    hmac: String,
}

#[wasm_bindgen]
impl Registration {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn hmac(&self) -> String {
        self.hmac.clone()
    }
}

/// One partial signature produced by `signPsbt`.
#[wasm_bindgen]
pub struct Signature {
    input_index: u32,
    pubkey: String,
    signature: String,
}

#[wasm_bindgen]
impl Signature {
    #[wasm_bindgen(getter, js_name = inputIndex)]
    pub fn input_index(&self) -> u32 {
        self.input_index
    }
    #[wasm_bindgen(getter)]
    pub fn pubkey(&self) -> String {
        self.pubkey.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn signature(&self) -> String {
        self.signature.clone()
    }
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
    pub async fn get_master_fingerprint(&self) -> Result<u32, JsValue> {
        let _g = CommandGuard::new();
        self.client
            .borrow_mut()
            .get_master_fingerprint(KeyTree::Standard)
            .await
            .map_err(map_err)
    }

    /// `GetExtendedPubkey` at `path` (e.g. `"m/84'/1'/0'"`). With `display`, awaits on-device
    /// confirmation. Resolves to the `xpub`/`tpub` string.
    #[wasm_bindgen(js_name = getExtendedPubkey)]
    pub async fn get_extended_pubkey(&self, path: String, display: bool) -> Result<String, JsValue> {
        let _g = CommandGuard::new();
        let (xpub, _sig) = self
            .client
            .borrow_mut()
            .get_extended_pubkey(KeyTree::Standard, &path, display, None)
            .await
            .map_err(map_err)?;
        Ok(bitcoin::base58::encode_check(&xpub))
    }

    /// `GetExtendedPubkey` for the i-th identity key (`m/1229210958'/i`). Pass a negative
    /// `index` for the unindexed key. With `display`, awaits on-device confirmation.
    #[wasm_bindgen(js_name = getIdentityKey)]
    pub async fn get_identity_key(&self, index: i32, display: bool) -> Result<String, JsValue> {
        let _g = CommandGuard::new();
        let idx = if index < 0 { None } else { Some(index as u32) };
        let xpub = self
            .client
            .borrow_mut()
            .get_identity_key(idx, display)
            .await
            .map_err(map_err)?;
        Ok(bitcoin::base58::encode_check(&xpub))
    }

    /// `RegisterIdentityKey`: registers a 33-byte compressed pubkey (hex) under `name`. Shows
    /// an on-device review.
    #[wasm_bindgen(js_name = registerIdentityKey)]
    pub async fn register_identity_key(
        &self,
        name: String,
        pubkey_hex: String,
    ) -> Result<Registration, JsValue> {
        let _g = CommandGuard::new();
        let bytes = hex::decode(pubkey_hex.trim()).map_err(|_| err_str("invalid pubkey hex"))?;
        let arr: [u8; 33] = bytes
            .try_into()
            .map_err(|_| err_str("pubkey must be 33 bytes"))?;
        let (id, hmac) = self
            .client
            .borrow_mut()
            .register_identity_key(&name, &arr)
            .await
            .map_err(map_err)?;
        Ok(registration(id.as_bytes(), &hmac.dangerous_as_bytes()))
    }

    /// `RegisterAccount`: registers a BIP-388 wallet policy (`descriptor_template` + newline/
    /// comma-separated `keys_info`) under `name`. Shows an on-device review.
    #[wasm_bindgen(js_name = registerAccount)]
    pub async fn register_account(
        &self,
        name: String,
        descriptor_template: String,
        keys_info: String,
        show_cleartext: bool,
    ) -> Result<Registration, JsValue> {
        let _g = CommandGuard::new();
        let account = Account::WalletPolicy(parse_wallet_policy(&descriptor_template, &keys_info)?);
        let (id, hmac) = self
            .client
            .borrow_mut()
            .register_account(&name, &account, None, None, show_cleartext)
            .await
            .map_err(map_err)?;
        Ok(registration(id.as_bytes(), &hmac.dangerous_as_bytes()))
    }

    /// `GetAddress` for a wallet policy at `(is_change, address_index)`. `por_hex` is the proof
    /// of registration from `registerAccount` (empty for a default policy). With `display`,
    /// awaits on-device confirmation. Resolves to the address.
    #[wasm_bindgen(js_name = getAddress)]
    #[allow(clippy::too_many_arguments)]
    pub async fn get_address(
        &self,
        descriptor_template: String,
        keys_info: String,
        name: String,
        is_change: bool,
        address_index: u32,
        por_hex: String,
        display: bool,
    ) -> Result<String, JsValue> {
        let _g = CommandGuard::new();
        let account = Account::WalletPolicy(parse_wallet_policy(&descriptor_template, &keys_info)?);
        let coords = AccountCoordinates::WalletPolicy(WalletPolicyCoordinates {
            is_change,
            address_index,
        });
        let por = parse_por(&por_hex)?;
        let (addr, _sig) = self
            .client
            .borrow_mut()
            .get_address(&account, &name, &coords, por.as_ref(), display, None)
            .await
            .map_err(map_err)?;
        Ok(addr)
    }

    /// `SignPsbt`: signs a base64 PSBTv0 with the given wallet policy. `name`/`por_hex` are the
    /// account name and proof of registration (from `registerAccount`) that authorize the
    /// policy. The PSBT is prepared with the policy's BIP-388 derivation info (as a real host
    /// would) before signing. Shows an on-device review. Resolves to the partial signatures.
    #[wasm_bindgen(js_name = signPsbt)]
    #[allow(clippy::too_many_arguments)]
    pub async fn sign_psbt(
        &self,
        psbt_base64: String,
        descriptor_template: String,
        keys_info: String,
        name: String,
        por_hex: String,
    ) -> Result<Vec<Signature>, JsValue> {
        let _g = CommandGuard::new();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(psbt_base64.trim())
            .map_err(|_| err_str("invalid base64 PSBT"))?;
        let mut psbt = bitcoin::psbt::Psbt::deserialize(&raw)
            .map_err(|e| err_str(format!("invalid PSBTv0: {e}")))?;
        let wp = parse_wallet_policy(&descriptor_template, &keys_info)?;
        let por: [u8; 32] = hex::decode(por_hex.trim())
            .map_err(|_| err_str("invalid por hex"))?
            .try_into()
            .map_err(|_| err_str("por must be 32 bytes"))?;
        // Inject the policy's BIP-388 key-derivation info so the device recognizes its inputs
        // (what a wallet/host does before handing a PSBT to the signer).
        common::psbt::prepare_psbt(&mut psbt, &[(&wp, name.as_str(), &por)])
            .map_err(|e| err_str(format!("prepare_psbt: {e:?}")))?;
        let v2 = vnd_bitcoin_client::psbt_v0_to_v2(&psbt.serialize())
            .map_err(|_| err_str("failed to convert PSBT to v2"))?;
        let signed = self
            .client
            .borrow_mut()
            .sign_psbt(&v2)
            .await
            .map_err(map_err)?;
        Ok(signed
            .signatures
            .iter()
            .map(|p| Signature {
                input_index: p.input_index,
                pubkey: hex::encode(&p.pubkey),
                signature: hex::encode(&p.signature),
            })
            .collect())
    }
}

fn parse_keys_info(keys_info: &str) -> Result<Vec<KeyInformation>, JsValue> {
    keys_info
        .split(['\n', ','])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| KeyInformation::try_from(s).map_err(|e| err_str(format!("invalid key info: {e:?}"))))
        .collect()
}

fn parse_wallet_policy(template: &str, keys_info: &str) -> Result<WalletPolicy, JsValue> {
    let keys = parse_keys_info(keys_info)?;
    WalletPolicy::new(template, keys).map_err(|e| err_str(format!("invalid wallet policy: {e:?}")))
}

fn parse_por(por_hex: &str) -> Result<Option<ProofOfRegistration<WalletPolicy>>, JsValue> {
    if por_hex.trim().is_empty() {
        return Ok(None);
    }
    let arr: [u8; 32] = hex::decode(por_hex.trim())
        .map_err(|_| err_str("invalid por hex"))?
        .try_into()
        .map_err(|_| err_str("por must be 32 bytes"))?;
    Ok(Some(ProofOfRegistration::from_bytes(arr)))
}

fn registration(id: &[u8], hmac: &[u8]) -> Registration {
    Registration {
        id: hex::encode(id),
        hmac: hex::encode(hmac),
    }
}

/// A plain JS `Error` for input/encoding failures.
fn err_str(msg: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&msg.to_string()).into()
}

/// Maps a client error to a JS `Error`, tagging its `name` so callers can branch on an
/// on-device rejection vs. a V-App error vs. anything else.
fn map_err(e: BitcoinClientError) -> JsValue {
    let name = match &e {
        BitcoinClientError::AppError(common::errors::Error::UserRejected) => "UserRejected",
        BitcoinClientError::AppError(_) => "AppError",
        _ => "BitcoinClientError",
    };
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}
