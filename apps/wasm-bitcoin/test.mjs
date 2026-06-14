// Node harness: the real Bitcoin V-App + the real BitcoinClient, co-resident in one wasm
// module, exercising both a request/response command and an *interactive* one driven by the
// cooperative step driver (the app awaits an on-device tap; we feed it from "JS").
import { readFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";

const bytes = readFileSync(
  new URL("./target/wasm32-unknown-unknown/debug/wasm_bitcoin.wasm", import.meta.url)
);
const module = new WebAssembly.Module(bytes);

const dec = new TextDecoder();
let mem;
const env = {
  host_print: (ptr, len) => console.log("[vapp]", dec.decode(new Uint8Array(mem.buffer, ptr, len))),
  host_random: (ptr, len) => crypto.getRandomValues(new Uint8Array(mem.buffer, ptr, len)),
  host_exit: (code) => { throw new Error("vapp exited: " + code); },
  host_fatal: (ptr, len) => { throw new Error("vapp fatal: " + dec.decode(new Uint8Array(mem.buffer, ptr, len))); },
};
const ex = new WebAssembly.Instance(module, { env }).exports;
mem = ex.memory;

const readResult = (n) => dec.decode(new Uint8Array(mem.buffer, ex.io_ptr(), Number(n)));

// Drive the in-flight command to completion. `onPending(n)` is called on each suspend (the
// app is awaiting on-device input); use it to feed events.
function driveToCompletion(onPending) {
  let pendings = 0;
  for (let i = 0; i < 10000; i++) {
    const r = ex.bitcoin_poll(); // i64 -> BigInt; -1 while waiting for input
    if (r !== -1n) return { result: readResult(r), pendings };
    pendings++;
    if (onPending) onPending(pendings);
  }
  throw new Error("command did not complete");
}

let failed = false;
const check = (cond, msg) => { console.log(`${cond ? "  OK  " : " FAIL "} ${msg}`); if (!cond) failed = true; };

ex.bitcoin_init();

// 1) Request/response: the real BitcoinClient.get_master_fingerprint over the real protocol.
ex.bitcoin_start_get_fingerprint();
let { result } = driveToCompletion();
console.log("get_master_fingerprint ->", result);
check(result === "fingerprint:f5acc2fd", "fingerprint matches the known master fingerprint");

// 2) GetExtendedPubkey without display: returns the actual extended pubkey (no UI).
ex.bitcoin_start_get_pubkey(0);
({ result } = driveToCompletion());
console.log("get_extended_pubkey(display=false) ->", result);
check(result.startsWith("xpub:tpub"), "returns a base58 extended pubkey");

// A flex (480x600) touch screen: tapping the "Next" location (399,568) advances the review,
// and on the final page that same spot lands inside the full-width "Confirm" button — so
// tapping it on every suspend navigates through and then approves. (76,28) is "Cancel".
const tap = (x, y) => { ex.bitcoin_push_touch(x, y, 1); ex.bitcoin_push_touch(x, y, 0); };

// 3) GetExtendedPubkey WITH display: the app draws an on-device confirmation and awaits a
//    tap. The command must suspend (poll returns -1) and the framebuffer must change; we
//    drive the review to its final "Confirm" and the extended pubkey comes back.
const fbBefore = ex.fb_version();
ex.bitcoin_start_get_pubkey(1);
let res3 = driveToCompletion(() => tap(399, 568));
console.log(`get_extended_pubkey(display=true, approve) -> ${res3.result}  (suspends=${res3.pendings})`);
check(res3.pendings >= 1, "interactive command suspended for on-device input (step driver)");
check(ex.fb_version() > fbBefore, "the app drew the confirmation screen to the framebuffer");
check(res3.result.startsWith("xpub:tpub"), "approving on-device returns the extended pubkey");

// 4) Same command, but reject by tapping "Cancel": the typed UserRejected error propagates
//    back through the real BitcoinClient.
ex.bitcoin_start_get_pubkey(1);
let res4 = driveToCompletion(() => tap(76, 28));
console.log(`get_extended_pubkey(display=true, reject) -> ${res4.result}  (suspends=${res4.pendings})`);
check(res4.result.startsWith("error:") && /reject/i.test(res4.result),
  "rejecting on-device yields a typed UserRejected error");

console.log(failed
  ? "\nFAIL"
  : "\nPASS: the real BitcoinClient drove the real Bitcoin V-App in wasm — request/response and an interactive on-device confirmation — via the cooperative step driver.");
process.exit(failed ? 1 : 0);
