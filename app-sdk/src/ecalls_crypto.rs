//! Target-agnostic crypto / bignum / hash ECALL implementations, shared by the native and
//! wasm backends — both are pure-Rust software implementations (k256, bip32, sha2, …). The
//! riscv backend instead routes these to the VM's crypto accelerator, so this file is only
//! compiled for the two software backends.
//!
//! Randomness is the only target-specific primitive: `rng_fill` dispatches to the active
//! backend's `fill_random` (native: `OsRng`; wasm: the JS host's `crypto.getRandomValues`).

use bip32::{ChildNumber, XPrv};
use hex_literal::hex;
use hmac::{Hmac, Mac};
use k256::{
    ecdsa::{self, signature::hazmat::PrehashVerifier},
    elliptic_curve::{
        sec1::{FromEncodedPoint, ToEncodedPoint},
        Group, PrimeField,
    },
    schnorr, EncodedPoint, ProjectivePoint, Scalar,
};
use num_bigint::BigUint;
use num_traits::Zero;
use sha2::Digest as _;
use sha2::Sha512;

use common::ecall_constants::{
    CurveKind, HashId, CTX_RIPEMD160_SIZE, CTX_SHA256_SIZE, CTX_SHA3_SIZE, CTX_SHA512_SIZE,
    MAX_BIGNUMBER_SIZE,
};

// default seed used in Speculos, corresponding to the mnemonic "glory promote mansion idle axis finger extra february uncover one trip resource lawn turtle enact monster seven myth punch hobby comfort wild raise skin"
const DEFAULT_SEED: [u8; 64] = hex!("b11997faff420a331bb4a4ffdc8bdc8ba7c01732a99a30d83dbbebd469666c84b47d09d3f5f472b3b9384ac634beba2a440ba36ec7661144132f35e206873564");

const SLIP21_MAGIC: &'static str = "Symmetric key seed";

/// Fills `buf` with cryptographically secure randomness from the active backend.
fn rng_fill(buf: &mut [u8]) {
    #[cfg(feature = "target_native")]
    crate::ecalls_native::fill_random(buf);
    #[cfg(feature = "target_wasm")]
    crate::ecalls_wasm::fill_random(buf);
}

unsafe fn to_bigint(bytes: *const u8, len: usize) -> BigUint {
    let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
    BigUint::from_bytes_be(bytes)
}

unsafe fn copy_result(r: *mut u8, result_bytes: &[u8], len: usize) -> () {
    unsafe {
        if result_bytes.len() < len {
            std::ptr::write_bytes(r, 0, len - result_bytes.len());
        }
        std::ptr::copy_nonoverlapping(
            result_bytes.as_ptr(),
            r.add(len - result_bytes.len()),
            result_bytes.len(),
        );
    }
}

pub fn bn_modm(r: *mut u8, n: *const u8, len: usize, m: *const u8, len_m: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE || len_m > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    if len_m > len_m {
        return 0;
    }

    let n = unsafe { to_bigint(n, len) };
    let m = unsafe { to_bigint(m, len_m) };

    if m.is_zero() {
        return 0;
    }

    let result = n % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_addm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = (a + b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_subm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    // the `+ &m` is to avoid negative numbers, since BigUints must be non-negative
    let result = ((a + &m) - b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_multm(r: *mut u8, a: *const u8, b: *const u8, m: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let b = unsafe { to_bigint(b, len) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m || b >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = (a * b) % &m;
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

/// Computes the modular inverse of `a` modulo `p`, storing the result in `r`.
/// The modulus `p` must be a prime number.
/// Uses Fermat's little theorem: a^{-1} = a^{p-2} mod p.
pub fn bn_modinv_prime(r: *mut u8, a: *const u8, p: *const u8, len: usize) -> u32 {
    if len > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let p = unsafe { to_bigint(p, len) };

    if a.is_zero() || p.is_zero() {
        return 0;
    }

    if a >= p {
        return 0;
    }

    // Fermat's little theorem: a^{-1} = a^{p-2} mod p (valid when p is prime)
    let exp = &p - BigUint::from(2u32);
    let result = a.modpow(&exp, &p);
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn bn_powm(
    r: *mut u8,
    a: *const u8,
    e: *const u8,
    len_e: usize,
    m: *const u8,
    len: usize,
) -> u32 {
    if len > MAX_BIGNUMBER_SIZE || len_e > MAX_BIGNUMBER_SIZE {
        return 0;
    }

    let a = unsafe { to_bigint(a, len) };
    let e = unsafe { to_bigint(e, len_e) };
    let m = unsafe { to_bigint(m, len) };

    if a >= m {
        return 0;
    }

    if m.is_zero() {
        return 0;
    }

    let result = a.modpow(&e, &m);
    let result_bytes = result.to_bytes_be();

    if result_bytes.len() > len {
        return 0;
    }

    unsafe {
        copy_result(r, &result_bytes, len);
    }

    1
}

pub fn derive_hd_node(
    curve: u32,
    path: *const u32,
    path_len: usize,
    privkey: *mut u8,
    chain_code: *mut u8,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }
    let mut key = get_master_bip32_key();

    let path_slice = unsafe { std::slice::from_raw_parts(path, path_len) };
    for path_step in path_slice {
        let child = ChildNumber::from(*path_step);
        key = match key.derive_child(child) {
            Ok(k) => k,
            Err(_) => return 0,
        };
    }

    // Copy the private key and chain code to the output buffers
    let privkey_bytes = key.private_key().to_bytes();
    let chain_code_bytes = key.attrs().chain_code;

    unsafe {
        std::ptr::copy_nonoverlapping(privkey_bytes.as_ptr(), privkey, privkey_bytes.len());
        std::ptr::copy_nonoverlapping(
            chain_code_bytes.as_ptr(),
            chain_code,
            chain_code_bytes.len(),
        );
    }

    1
}

pub fn get_master_fingerprint(curve: u32) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    u32::from_be_bytes(get_master_bip32_key().public_key().fingerprint())
}

pub fn derive_slip21_node(labels: *const u8, labels_len: usize, out: *mut u8) -> u32 {
    if out.is_null() {
        return 0;
    }

    // Vanadium uses a custom seed for its SLIP-21 hierarchy, for compatibility with Bolos
    // The seed is derived from a master secret using the standard SLIP-21 derivation.
    let custom_slip21_seed = slip21_custom_get_seed();

    let mut current_node = slip21_get_master_node(&custom_slip21_seed);

    if labels_len > 256 {
        return 0;
    }

    let labels: &[u8] = unsafe { std::slice::from_raw_parts(labels, labels_len) };

    // parse the `labels` buffer as the concatenation of a list of labels, each prefixed by its length
    // The length of each label is between 0 and 252 bytes, and the total length of the labels buffer must be
    // at most 256 bytes.

    let mut offset = 0;
    while offset < labels_len {
        if offset >= labels_len {
            return 0; // Buffer underrun
        }

        let label_len = labels[offset] as usize;
        offset += 1;

        if label_len > 252 {
            return 0; // Label too long
        }

        if offset + label_len > labels_len {
            return 0; // Buffer overrun
        }

        let label = &labels[offset..offset + label_len];
        offset += label_len;

        current_node = slip21_derive_child_node(&current_node, label);
    }

    unsafe {
        std::ptr::copy_nonoverlapping(current_node.as_ptr(), out, current_node.len());
    }

    1
}

pub fn ecfp_add_point(curve: u32, r: *mut u8, p: *const u8, q: *const u8) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        return 0;
    }

    let p_slice = unsafe { std::slice::from_raw_parts(p, 65) };
    let q_slice = unsafe { std::slice::from_raw_parts(q, 65) };

    // Helper to validate a point and copy it to the result
    let validate_and_copy = |point_slice: &[u8], source: *const u8| -> u32 {
        let encoded = match EncodedPoint::from_bytes(point_slice) {
            Ok(enc) => enc,
            Err(_) => return 0,
        };
        if ProjectivePoint::from_encoded_point(&encoded)
            .is_none()
            .into()
        {
            return 0;
        }
        // Use ptr::copy (memmove semantics) to handle the case where source == r.
        unsafe {
            std::ptr::copy(source, r, 65);
        }
        1
    };

    // Handle point at infinity: represented as prefix byte 0x00
    match (p_slice[0] == 0x00, q_slice[0] == 0x00) {
        (true, true) => {
            unsafe {
                std::ptr::write_bytes(r, 0, 65);
            }
            return 1;
        }
        (true, false) => return validate_and_copy(q_slice, q),
        (false, true) => return validate_and_copy(p_slice, p),
        (false, false) => { /* Continue with normal addition */ }
    }

    // Validate and decode point P
    let p_point = match EncodedPoint::from_bytes(p_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let p_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&p_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    // Validate and decode point Q
    let q_point = match EncodedPoint::from_bytes(q_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let q_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&q_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    let result_point: ProjectivePoint = p_point + q_point;

    // Check if result is the point at infinity
    // The k256 library may panic when encoding the point at infinity,
    // so we check for it first
    if bool::from(result_point.is_identity()) {
        // Encode point at infinity as all zeros (65 bytes)
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    let result_encoded = result_point.to_encoded_point(false);
    let result_bytes = result_encoded.as_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, result_bytes.len());
    }

    1
}

pub fn ecfp_scalar_mult(curve: u32, r: *mut u8, p: *const u8, k: *const u8, k_len: usize) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        return 0;
    }
    if k_len > 32 {
        return 0;
    }

    let p_slice = unsafe { std::slice::from_raw_parts(p, 65) };
    let k_slice = unsafe { std::slice::from_raw_parts(k, k_len) };

    // Handle point at infinity: represented as prefix byte 0x00
    if p_slice[0] == 0x00 {
        // O * k = O
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    // Validate and decode point P
    let p_point = match EncodedPoint::from_bytes(p_slice) {
        Ok(enc) => enc,
        Err(_) => return 0,
    };
    let p_point: ProjectivePoint = match ProjectivePoint::from_encoded_point(&p_point).into() {
        Some(pt) => pt,
        None => return 0,
    };

    // pad k_scalar to 32 bytes with initial zeros without using unsafe code
    let mut k_scalar = [0u8; 32];
    k_scalar[32 - k_len..].copy_from_slice(k_slice);
    let k_scalar: Scalar = match Scalar::from_repr(k_scalar.into()).into() {
        Some(scalar) => scalar,
        None => return 0,
    };

    let result_point: ProjectivePoint = p_point * k_scalar;

    // Check if result is the point at infinity
    if bool::from(result_point.is_identity()) {
        // Encode point at infinity as all zeros (65 bytes)
        unsafe {
            std::ptr::write_bytes(r, 0, 65);
        }
        return 1;
    }

    let result_encoded = result_point.to_encoded_point(false);
    let result_bytes = result_encoded.as_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, result_bytes.len());
    }

    1
}

pub fn get_random_bytes(buffer: *mut u8, size: usize) -> u32 {
    if size == 0 {
        return 1;
    }
    if size > 256 {
        panic!("size is too large");
    }

    let mut random_bytes = [0u8; 256];
    rng_fill(&mut random_bytes[..size]);

    unsafe {
        std::ptr::copy_nonoverlapping(random_bytes.as_ptr(), buffer, size);
    }

    1
}

pub fn ecdsa_sign(
    curve: u32,
    mode: u32,
    hash_id: u32,
    privkey: *const u8,
    msg_hash: *const u8,
    signature: *mut u8,
) -> usize {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::EcdsaSignMode::RFC6979 as u32 {
        panic!("Invalid or unsupported ecdsa signing mode");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    let privkey_slice = unsafe { std::slice::from_raw_parts(privkey, 32) };
    let msg_hash_slice = unsafe { std::slice::from_raw_parts(msg_hash, 32) };

    let mut privkey_bytes = [0u8; 32];
    privkey_bytes[..].copy_from_slice(privkey_slice);
    let signing_key =
        ecdsa::SigningKey::from_bytes(&privkey_bytes.into()).expect("Invalid private key");
    let (signature_local, _) = signing_key
        .sign_prehash_recoverable(msg_hash_slice)
        .expect("Signing failed");

    let signature_der = ecdsa::DerSignature::from(signature_local);

    let signature_bytes = signature_der.to_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(signature_bytes.as_ptr(), signature, signature_bytes.len());
    }

    signature_bytes.len()
}

pub fn ecdsa_verify(
    curve: u32,
    pubkey: *const u8,
    msg_hash: *const u8,
    signature: *const u8,
    signature_len: usize,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if signature_len > 72 {
        panic!("signature_len is too large");
    }

    let pubkey_slice = unsafe { std::slice::from_raw_parts(pubkey, 65) };
    let msg_hash_slice = unsafe { std::slice::from_raw_parts(msg_hash, 32) };
    let signature_slice = unsafe { std::slice::from_raw_parts(signature, signature_len) };

    let pubkey_point = EncodedPoint::from_bytes(pubkey_slice).expect("Invalid public key");
    let verifying_key = ecdsa::VerifyingKey::from_encoded_point(&pubkey_point)
        .expect("Failed to create verifying key");

    let signature =
        ecdsa::DerSignature::from_bytes(signature_slice.into()).expect("Invalid signature");

    match verifying_key.verify_prehash(msg_hash_slice, &signature) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

pub fn schnorr_sign(
    curve: u32,
    mode: u32,
    hash_id: u32,
    privkey: *const u8,
    msg: *const u8,
    msg_len: usize,
    signature: *mut u8,
    entropy: *const [u8; 32],
) -> usize {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::SchnorrSignMode::BIP340 as u32 {
        panic!("Invalid or unsupported schnorr signing mode");
    }

    if msg_len > 128 {
        panic!("msg_len is too large");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    let privkey_slice = unsafe { std::slice::from_raw_parts(privkey, 32) };
    let msg_slice = unsafe { std::slice::from_raw_parts(msg, msg_len) };

    let mut privkey_bytes = [0u8; 32];
    privkey_bytes[..].copy_from_slice(privkey_slice);
    let signing_key = schnorr::SigningKey::from_bytes(&privkey_bytes).expect("Invalid private key");

    let aux_rand = if entropy.is_null() {
        // generate 32 random bytes
        let mut aux_rand = [0u8; 32];
        rng_fill(&mut aux_rand);
        aux_rand
    } else {
        unsafe { *entropy }
    };

    let signature_bytes = signing_key
        .sign_raw(msg_slice, &aux_rand)
        .unwrap()
        .to_bytes();

    unsafe {
        std::ptr::copy_nonoverlapping(signature_bytes.as_ptr(), signature, signature_bytes.len());
    }

    signature_bytes.len()
}

pub fn schnorr_verify(
    curve: u32,
    mode: u32,
    hash_id: u32,
    pubkey: *const u8,
    msg: *const u8,
    msg_len: usize,
    signature: *const u8,
    signature_len: usize,
) -> u32 {
    if curve != CurveKind::Secp256k1 as u32 {
        panic!("Unsupported curve");
    }

    if mode != common::ecall_constants::SchnorrSignMode::BIP340 as u32 {
        panic!("Invalid or unsupported schnorr signing mode");
    }

    if msg_len > 128 {
        panic!("msg_len is too large");
    }

    if hash_id != common::ecall_constants::HashId::Sha256 as u32 {
        panic!("Invalid or unsupported hash id");
    }

    if signature_len != 64 {
        panic!("Invalid signature length");
    }

    let pubkey_slice = unsafe { std::slice::from_raw_parts(pubkey, 65) };
    let xonly_pubkey_slice = &pubkey_slice[1..33];
    let msg_slice = unsafe { std::slice::from_raw_parts(msg, msg_len) };
    let signature_slice = unsafe { std::slice::from_raw_parts(signature, signature_len) };

    let verifying_key =
        schnorr::VerifyingKey::from_bytes(xonly_pubkey_slice).expect("Invalid public key");
    let signature = schnorr::Signature::try_from(signature_slice).expect("Invalid signature");

    match verifying_key.verify_raw(msg_slice, &signature) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

fn get_master_bip32_key() -> XPrv {
    XPrv::new(&DEFAULT_SEED).expect("Failed to create master key from seed")
}

// custom master seed used in Vanadium's version of SLIP-21 for compatibility with Bolos
const SEED_MASTER_PATH: &'static str = "VANADIUM";
fn slip21_custom_get_seed() -> [u8; 32] {
    let m = slip21_get_master_node(&DEFAULT_SEED);
    let c = slip21_derive_child_node(&m, SEED_MASTER_PATH.as_bytes());
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&c[32..64]);
    seed
}

pub(crate) fn slip21_get_master_node(seed: &[u8]) -> [u8; 64] {
    // compute HMAC-SHA512(key = SLIP21_MAGIC, msg = seed)
    let mut mac = Hmac::<Sha512>::new_from_slice(SLIP21_MAGIC.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(seed);
    mac.finalize().into_bytes().into()
}

pub(crate) fn slip21_derive_child_node(cur_node: &[u8; 64], label: &[u8]) -> [u8; 64] {
    // compute HMAC-SHA512(key = cur_node[:32], msg = [0] + label)
    let mut mac =
        Hmac::<Sha512>::new_from_slice(&cur_node[..32]).expect("HMAC can take key of any size");
    mac.update(&[0u8]);
    mac.update(label);
    mac.finalize().into_bytes().into()
}

const _: () = assert!(
    std::mem::size_of::<sha2::Sha256>() <= CTX_SHA256_SIZE,
    "sha2::Sha256 does not fit in CtxSha256",
);
const _: () = assert!(
    std::mem::size_of::<sha2::Sha512>() <= CTX_SHA512_SIZE,
    "sha2::Sha512 does not fit in CtxSha512",
);
const _: () = assert!(
    std::mem::size_of::<ripemd::Ripemd160>() <= CTX_RIPEMD160_SIZE,
    "ripemd::Ripemd160 does not fit in CtxRipemd160",
);
// All sha3/keccak variants wrap the same Keccak-f[1600] state, so checking one representative
// of each family is sufficient.
const _: () = assert!(
    std::mem::size_of::<sha3::Keccak256>() <= CTX_SHA3_SIZE,
    "sha3::Keccak256 does not fit in CTX_SHA3_SIZE",
);
const _: () = assert!(
    std::mem::size_of::<sha3::Sha3_256>() <= CTX_SHA3_SIZE,
    "sha3::Sha3_256 does not fit in CTX_SHA3_SIZE",
);

pub fn hash_init(hash_identifier: u32, ctx: *mut u8) {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                panic!("hash_init: invalid output size {} for SHA-256", output_size);
            }
            let hasher = sha2::Sha256::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha256, hasher) };
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                panic!("hash_init: invalid output size {} for SHA-512", output_size);
            }
            let hasher = sha2::Sha512::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha512, hasher) };
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                panic!(
                    "hash_init: invalid output size {} for RIPEMD-160",
                    output_size
                );
            }
            let hasher = ripemd::Ripemd160::new();
            unsafe { std::ptr::write_unaligned(ctx as *mut ripemd::Ripemd160, hasher) };
        }
        id if id == HashId::Keccak as u32 => match output_size {
            28 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak224, sha3::Keccak224::new())
            },
            32 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak256, sha3::Keccak256::new())
            },
            48 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak384, sha3::Keccak384::new())
            },
            64 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Keccak512, sha3::Keccak512::new())
            },
            _ => panic!(
                "hash_init: invalid output size {} for Keccak (must be 28, 32, 48 or 64)",
                output_size
            ),
        },
        id if id == HashId::Sha3 as u32 => match output_size {
            28 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_224, sha3::Sha3_224::new())
            },
            32 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_256, sha3::Sha3_256::new())
            },
            48 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_384, sha3::Sha3_384::new())
            },
            64 => unsafe {
                std::ptr::write_unaligned(ctx as *mut sha3::Sha3_512, sha3::Sha3_512::new())
            },
            _ => panic!(
                "hash_init: invalid output size {} for SHA-3 (must be 28, 32, 48 or 64)",
                output_size
            ),
        },
        _ => panic!("hash_init: unsupported hash_id {}", hash_id),
    }
}

pub fn hash_update(hash_identifier: u32, ctx: *mut u8, data: *const u8, len: usize) -> u32 {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    let data_slice = unsafe { std::slice::from_raw_parts(data, len) };
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha256) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha256, hasher) };
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha512) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut sha2::Sha512, hasher) };
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                return 0;
            }
            let mut hasher = unsafe { std::ptr::read_unaligned(ctx as *const ripemd::Ripemd160) };
            hasher.update(data_slice);
            unsafe { std::ptr::write_unaligned(ctx as *mut ripemd::Ripemd160, hasher) };
        }
        id if id == HashId::Keccak as u32 => match output_size {
            28 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak224) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak224, h) };
            }
            32 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak256) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak256, h) };
            }
            48 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak384) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak384, h) };
            }
            64 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Keccak512) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Keccak512, h) };
            }
            _ => return 0,
        },
        id if id == HashId::Sha3 as u32 => match output_size {
            28 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_224) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_224, h) };
            }
            32 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_256) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_256, h) };
            }
            48 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_384) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_384, h) };
            }
            64 => {
                let mut h = unsafe { std::ptr::read_unaligned(ctx as *const sha3::Sha3_512) };
                h.update(data_slice);
                unsafe { std::ptr::write_unaligned(ctx as *mut sha3::Sha3_512, h) };
            }
            _ => return 0,
        },
        _ => return 0, // Unsupported hash_id
    }
    1
}

pub fn hash_final(hash_identifier: u32, ctx: *mut u8, digest: *mut u8) -> u32 {
    let output_size = (hash_identifier & 0xFFFF) as usize; // requested output size from low 16 bits
    let hash_id = hash_identifier >> 16; // extract algorithm part from composite ecall_id
    match hash_id {
        id if id == HashId::Sha256 as u32 => {
            if output_size != 32 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha256) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 32);
            }
        }
        id if id == HashId::Sha512 as u32 => {
            if output_size != 64 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const sha2::Sha512) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 64);
            }
        }
        id if id == HashId::Ripemd160 as u32 => {
            if output_size != 20 {
                return 0;
            }
            let hasher = unsafe { std::ptr::read_unaligned(ctx as *const ripemd::Ripemd160) };
            let result = hasher.finalize();
            unsafe {
                std::ptr::copy_nonoverlapping(result.as_ptr(), digest as *mut u8, 20);
            }
        }
        id if id == HashId::Keccak as u32 => {
            macro_rules! keccak_final {
                ($ty:ty, $len:expr) => {{
                    let h = unsafe { std::ptr::read_unaligned(ctx as *const $ty) };
                    let result = h.finalize();
                    unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), digest, $len) };
                }};
            }
            match output_size {
                28 => keccak_final!(sha3::Keccak224, 28),
                32 => keccak_final!(sha3::Keccak256, 32),
                48 => keccak_final!(sha3::Keccak384, 48),
                64 => keccak_final!(sha3::Keccak512, 64),
                _ => return 0,
            }
        }
        id if id == HashId::Sha3 as u32 => {
            macro_rules! sha3_final {
                ($ty:ty, $len:expr) => {{
                    let h = unsafe { std::ptr::read_unaligned(ctx as *const $ty) };
                    let result = h.finalize();
                    unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), digest, $len) };
                }};
            }
            match output_size {
                28 => sha3_final!(sha3::Sha3_224, 28),
                32 => sha3_final!(sha3::Sha3_256, 32),
                48 => sha3_final!(sha3::Sha3_384, 48),
                64 => sha3_final!(sha3::Sha3_512, 64),
                _ => return 0,
            }
        }
        _ => return 0, // Unsupported hash_id
    }
    1
}
