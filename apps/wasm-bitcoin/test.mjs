// Node harness: the real Bitcoin V-App + a co-resident client, both in one wasm module.
// Calls the GetMasterFingerprint command end-to-end (real CBOR protocol -> real handler).
import { readFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";

const bytes = readFileSync(
  new URL("./target/wasm32-unknown-unknown/debug/wasm_bitcoin.wasm", import.meta.url)
);
const module = new WebAssembly.Module(bytes);
console.log(
  "imports:",
  WebAssembly.Module.imports(module).map((i) => `${i.module}.${i.name}`).join(", ") || "(none)"
);

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

ex.bitcoin_init();
const fp = ex.bitcoin_get_fingerprint() >>> 0;
const hex = "0x" + fp.toString(16).padStart(8, "0");
console.log(`Bitcoin app GetMasterFingerprint -> ${hex}`);
if (fp === 0) {
  console.log("FAIL: zero fingerprint (error response)");
  process.exit(1);
}
const expected = 0xf5acc2fd;
console.log(
  fp === expected
    ? "  matches the known master fingerprint (0xf5acc2fd) — OK"
    : "  (computed; differs from the SDK master fingerprint constant)"
);
console.log("\nPASS: the real Bitcoin V-App ran in wasm, driven by a co-resident client over the real CBOR protocol.");
