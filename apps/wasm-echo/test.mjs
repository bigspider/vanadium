// Node harness that drives the wasm V-App exactly as a web page would: provide the host
// imports, then push a command through the IO buffer and read the response back.
import { readFileSync, writeFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";
import { deflateSync } from "node:zlib";

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

// --- the display side: the handler drew the idle screen; read the framebuffer ---
const w = ex.fb_width(), h = ex.fb_height();
const fb = new Uint8Array(mem.buffer, ex.fb_ptr(), w * h);
const hist = new Map();
for (const v of fb) hist.set(v, (hist.get(v) ?? 0) + 1);
const distinct = [...hist.keys()].sort((a, b) => a - b);
let bg = 0, bgCount = -1;
for (const [v, c] of hist) if (c > bgCount) { bg = v; bgCount = c; }
const nonbg = w * h - bgCount;
console.log(`\nframebuffer: ${w}x${h}  version=${ex.fb_version()}`);
console.log(`distinct intensities: [${distinct.join(", ")}]  non-background pixels: ${nonbg}`);
if (nonbg === 0 || distinct.length < 2) { console.log("FAIL: framebuffer is blank"); process.exit(1); }

// Render to a PNG so the draw can be eyeballed.
const png = encodePng(w, h, fb);
writeFileSync(new URL("./wasm_fb.png", import.meta.url), png);
console.log("wrote wasm_fb.png");
console.log("\nPASS: V-App ran in wasm, round-tripped from JS, and drew to the framebuffer.");

// Minimal grayscale PNG encoder (intensity 0..15 -> 0..255).
function encodePng(w, h, px) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  let o = 0;
  for (let y = 0; y < h; y++) {
    raw[o++] = 0; // filter: none
    for (let x = 0; x < w; x++) {
      const g = Math.round((px[y * w + x] * 255) / 15);
      raw[o++] = g; raw[o++] = g; raw[o++] = g;
    }
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(body) >>> 0);
    return Buffer.concat([len, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 2; // 8-bit, RGB
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
function crc32(buf) {
  let c = ~0;
  for (let i = 0; i < buf.length; i++) {
    c ^= buf[i];
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return ~c;
}
