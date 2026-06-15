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
  promise.then((v) => { res = v; settled = true; }, (e) => { err = (e && e.message) || String(e); settled = true; });
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

// 3) Interactive: reject by tapping Cancel — the Promise rejects with the typed error.
const rejected = await driveWithTaps(app.getExtendedPubkey("m/84'/1'/0'", true), 76, 28);
console.log("getExtendedPubkey(reject) ->", rejected.res || rejected.err);
check(rejected.res === undefined && /reject/i.test(rejected.err || ""),
  "rejecting on-device rejects the Promise with a UserRejected error");

console.log(failed
  ? "\nFAIL"
  : "\nPASS: the real BitcoinClient, compiled to JS via wasm-bindgen, drove the real Bitcoin V-App — request/response and interactive on-device confirmation.");
process.exit(failed ? 1 : 0);
