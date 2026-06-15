// Node test: the real BitcoinClient, compiled to JS via wasm-bindgen, driving the real
// Bitcoin V-App co-resident in the same wasm module. Exercises a request/response command and
// an interactive one (on-device confirmation, driven by feeding taps). Run `./build.sh` first
// (it generates ./pkg-node).
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const wb = require("./pkg-node/vnd_bitcoin.js");

let failed = false;
const check = (c, m) => { console.log(`${c ? "  OK  " : " FAIL "} ${m}`); if (!c) failed = true; };
const sleep = () => new Promise((r) => setTimeout(r, 0));

// Drive an in-flight command Promise to settlement by tapping (x,y) each tick. On a flex
// screen (399,568) is the Next button, which on the final review page is the Confirm button;
// (76,28) is Cancel.
async function driveWithTaps(promise, x, y) {
  let res, err, settled = false;
  promise.then((v) => { res = v; settled = true; }, (e) => { err = e; settled = true; });
  for (let i = 0; i < 60 && !settled; i++) {
    wb.vappPushTouch(x, y, true);
    wb.vappPushTouch(x, y, false);
    await sleep();
  }
  return { res, err };
}

// 0) Crypto + RNG path: BIP340 schnorr sign x2 (aux randomness from crypto.getRandomValues)
//    + verify both. Proves signing and randomness work in wasm (the commands below are
//    deterministic and never touch the RNG).
check(wb.vappCryptoSelfTest(), "crypto self-test (schnorr sign/verify + getRandomValues) passes");

const app = new wb.BitcoinApp();

// 1) Request/response — the real BitcoinClient.get_master_fingerprint, as a JS Promise.
const fp = (await app.getMasterFingerprint()) >>> 0;
console.log("getMasterFingerprint ->", "0x" + fp.toString(16));
check(fp === 0xf5acc2fd, "fingerprint matches the known master fingerprint");

// 2) Interactive: approve the on-device confirmation by tapping through to Confirm.
const approved = await driveWithTaps(app.getExtendedPubkey("m/84'/1'/0'", true), 399, 568);
console.log("getExtendedPubkey(approve) ->", approved.res || approved.err);
check(typeof approved.res === "string" && approved.res.startsWith("tpub"),
  "approving on-device returns the extended pubkey");

// 3) Interactive: reject by tapping Cancel — the Promise rejects with an Error whose `name`
//    is "UserRejected" (so callers can branch on it without matching message text).
const rejected = await driveWithTaps(app.getExtendedPubkey("m/84'/1'/0'", true), 76, 28);
console.log("getExtendedPubkey(reject) ->", rejected.err && rejected.err.name + ": " + rejected.err.message);
check(rejected.res === undefined && rejected.err && rejected.err.name === "UserRejected",
  "rejecting on-device rejects with a named UserRejected error");

// A BIP-388 single-sig wallet policy on the default test seed (master fpr f5acc2fd).
const WP_TEMPLATE = "wpkh(@0/**)";
const WP_KEYS = "[f5acc2fd/84'/1'/0']tpubDCtKfsNyRhULjZ9XMS4VKKtVcPdVDi8MKUbcSD9MJDyjRu1A2ND5MiipozyyspBT9bg8upEp7a8EAgFxNxXn1d7QkdbL52Ty5jiSLcxPt1P";
const NAME = "Segwit account";

// 4) getIdentityKey (non-interactive).
const idk = await app.getIdentityKey(-1, false);
console.log("getIdentityKey ->", idk);
check(typeof idk === "string" && idk.startsWith("tpub"), "getIdentityKey returns an extended pubkey");

// 5) registerAccount (interactive approve) -> proof of registration; then getAddress (no UI)
//    returns the known address for (is_change=false, index=0).
const reg = await driveWithTaps(app.registerAccount(NAME, WP_TEMPLATE, WP_KEYS, false), 399, 568);
const regObj = reg.res; // a typed Registration { id, hmac }
console.log("registerAccount ->", regObj ? { id: regObj.id, hmac: regObj.hmac } : reg.err);
check(regObj && /^[0-9a-f]{64}$/.test(regObj.hmac), "registerAccount returns a 32-byte proof of registration");

const addr = await app.getAddress(WP_TEMPLATE, WP_KEYS, NAME, false, 0, regObj ? regObj.hmac : "", false);
console.log("getAddress ->", addr);
check(addr === "tb1qzdr7s2sr0dwmkwx033r4nujzk86u0cy6fmzfjk", "getAddress returns the expected address");

// 6) registerIdentityKey (interactive approve) with the secp256k1 generator as a sample pubkey.
const GEN = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const rik = await driveWithTaps(app.registerIdentityKey("My key", GEN), 399, 568);
const rikObj = rik.res; // a typed Registration { id, hmac }
console.log("registerIdentityKey ->", rikObj ? { id: rikObj.id, hmac: rikObj.hmac } : rik.err);
check(rikObj && /^[0-9a-f]{64}$/.test(rikObj.hmac), "registerIdentityKey returns a 32-byte proof of registration");

// 7) signPsbt (interactive approve): a single-sig wpkh PSBT signed with the policy registered
//    above (its proof of registration authorizes the device's inputs).
const PSBT = "cHNidP8BAHQCAAAAAXoqmXlWwJ+Op/0oGcGph7sU4iv5rc2vIKiXY3Is7uJkAQAAAAD9////AqC7DQAAAAAAGXapFDRKD0jKFQ7CuQOBdmC5tosTpnAmiKx0OCMAAAAAABYAFOs4+puBKPgfJule2wxf+uqDaQ/kAAAAAAABAH0CAAAAAa+/rgZZD3Qf8a9ZtqxGESYzakxKgttVPfb++rc3rDPzAQAAAAD9////AnARAQAAAAAAIgAg/e5EHFblsG0N+CwSTHBwFKXKGWWL4LmFa8oW8e0yWfel9DAAAAAAABYAFDr4QprVlUql7oozyYP9ih6GeZJLAAAAAAEBH6X0MAAAAAAAFgAUOvhCmtWVSqXuijPJg/2KHoZ5kksiBgPuLD2Y6x+TwKGqjlpACbcOt7ROrRXxZm8TawEq1Y0waBj1rML9VAAAgAEAAIAAAACAAQAAAAgAAAAAACICAinsR3JxMe0liKIMRu2pq7fapvSf1Quv5wucWqaWHE7MGPWswv1UAACAAQAAgAAAAIABAAAACgAAAAA=";
const sp = await driveWithTaps(app.signPsbt(PSBT, WP_TEMPLATE, WP_KEYS, NAME, regObj ? regObj.hmac : ""), 399, 568);
const sigs = sp.res; // a typed Signature[] (each { inputIndex, pubkey, signature })
console.log("signPsbt ->", sigs ? sigs.map((s) => ({ inputIndex: s.inputIndex, signature: s.signature })) : sp.err);
check(Array.isArray(sigs) && sigs.length >= 1 && /^[0-9a-f]+$/.test(sigs[0].signature),
  "signPsbt returns at least one signature");

console.log(failed
  ? "\nFAIL"
  : "\nPASS: the real BitcoinClient, compiled to JS via wasm-bindgen, drove the real Bitcoin V-App — fingerprint, pubkey, identity key, register account, get address, register identity key, and sign PSBT.");
process.exit(failed ? 1 : 0);
