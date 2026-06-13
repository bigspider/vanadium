// Node harness driving the interactive V-App exactly as the page would: start a command,
// poll, feed touch events, poll, ... until the handler finishes. Validates the stored-
// future step driver: a handler that awaits get_event runs in wasm, suspending back to JS
// between inputs.
import { readFileSync, writeFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";
import { deflateSync } from "node:zlib";

const wasmPath = "./target/wasm32-unknown-unknown/debug/wasm_echo.wasm";
const bytes = readFileSync(new URL(wasmPath, import.meta.url));
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

ex.app_init();
ex.app_start(0); // empty command -> starts the interactive counter

// First poll: handler draws "Taps: 0" then suspends waiting for input.
let r = ex.app_poll();
if (r !== -1n && r !== -1) { console.log("FAIL: expected pending after start, got", r); process.exit(1); }

// Tap 5 times; each (push + poll) increments the counter and re-suspends.
const taps = 5;
for (let i = 0; i < taps; i++) {
  ex.app_push_touch(100, 100, 1);
  r = ex.app_poll();
  if (r !== -1n && r !== -1) { console.log("FAIL: command ended early"); process.exit(1); }
}
console.log(`fed ${taps} taps, handler still running (suspended for input): OK`);

// Snapshot the framebuffer (should read "Taps: 5").
const w = ex.fb_width(), h = ex.fb_height();
const fb = new Uint8Array(mem.buffer, ex.fb_ptr(), w * h);
const nonbg = fb.reduce((n, v) => n + (v !== 15 ? 1 : 0), 0);
console.log(`framebuffer ${w}x${h} version=${ex.fb_version()} non-background pixels=${nonbg}`);
if (nonbg === 0) { console.log("FAIL: framebuffer blank"); process.exit(1); }
writeFileSync(new URL("./wasm_fb.png", import.meta.url), encodePng(w, h, fb));
console.log("wrote wasm_fb.png");

// Quit: the handler's event loop breaks and returns its response.
ex.app_push_quit();
r = ex.app_poll();
if (r === -1n || r === -1) { console.log("FAIL: expected a response after quit"); process.exit(1); }
const resp = dec.decode(new Uint8Array(mem.buffer, ex.io_ptr(), Number(r)));
const ok = resp === `count=${taps}`;
console.log(`final response: ${JSON.stringify(resp)}  ${ok ? "OK" : `MISMATCH (want count=${taps})`}`);
if (!ok) process.exit(1);
console.log("\nPASS: interactive V-App handler ran in wasm, suspended for input, and finished.");

// Minimal grayscale PNG encoder (intensity 0..15 -> 0..255).
function encodePng(w, h, px) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  let o = 0;
  for (let y = 0; y < h; y++) {
    raw[o++] = 0;
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
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0)),
  ]);
}
function crc32(buf) {
  let c = ~0;
  for (let i = 0; i < buf.length; i++) { c ^= buf[i]; for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1)); }
  return ~c;
}
