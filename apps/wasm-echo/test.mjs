// Node harness that drives the wasm V-App exactly as a web page would: provide the host
// imports, then push a command through the IO buffer and read the response back.
import { readFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";

const wasmPath = "./target/wasm32-unknown-unknown/debug/wasm_echo.wasm";
const bytes = readFileSync(new URL(wasmPath, import.meta.url));
const module = new WebAssembly.Module(bytes);

console.log("imports the module needs:");
for (const i of WebAssembly.Module.imports(module)) {
  console.log(`  ${i.module}.${i.name} (${i.kind})`);
}

const dec = new TextDecoder();
const enc = new TextEncoder();
let mem;
const env = {
  host_print: (ptr, len) =>
    console.log("[vapp]", dec.decode(new Uint8Array(mem.buffer, ptr, len))),
  host_random: (ptr, len) =>
    crypto.getRandomValues(new Uint8Array(mem.buffer, ptr, len)),
  host_exit: (code) => {
    throw new Error("vapp exited with status " + code);
  },
  host_fatal: (ptr, len) => {
    throw new Error("vapp fatal: " + dec.decode(new Uint8Array(mem.buffer, ptr, len)));
  },
};

const instance = new WebAssembly.Instance(module, { env });
const ex = instance.exports;
mem = ex.memory;

ex.app_init();

function send(str) {
  const buf = enc.encode(str);
  if (buf.length > ex.io_cap()) throw new Error("command too large");
  new Uint8Array(mem.buffer, ex.io_ptr(), buf.length).set(buf);
  const n = ex.app_send(buf.length);
  return dec.decode(new Uint8Array(mem.buffer, ex.io_ptr(), n));
}

for (const cmd of ["hello vanadium", "second message", ""]) {
  const reply = send(cmd);
  const ok = reply === "echo:" + cmd;
  console.log(`send(${JSON.stringify(cmd)}) -> ${JSON.stringify(reply)}  ${ok ? "OK" : "MISMATCH"}`);
  if (!ok) process.exit(1);
}
console.log("\nPASS: V-App handler ran inside wasm and round-tripped from JS.");
