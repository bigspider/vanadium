// Dumps a few device frames to PNG so the rendering can be eyeballed: the startup dashboard,
// the on-device xpub confirmation, and the "verified" screen. No deps (PNG via node:zlib).
import { readFileSync, writeFileSync } from "node:fs";
import { webcrypto as crypto } from "node:crypto";
import { deflateSync } from "node:zlib";

const ex = (() => {
  const bytes = readFileSync(new URL("./target/wasm32-unknown-unknown/debug/wasm_bitcoin.wasm", import.meta.url));
  const dec = new TextDecoder();
  let e;
  const u8 = (p, l) => new Uint8Array(e.memory.buffer, p, l);
  const env = {
    host_print: (p, l) => console.log("[vapp]", dec.decode(u8(p, l))),
    host_random: (p, l) => crypto.getRandomValues(u8(p, l)),
    host_exit: (c) => { throw new Error("exit " + c); },
    host_fatal: (p, l) => { throw new Error("fatal " + dec.decode(u8(p, l))); },
  };
  e = new WebAssembly.Instance(new WebAssembly.Module(bytes), { env }).exports;
  return e;
})();

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
function chunk(type, data) {
  const t = Buffer.from(type, "ascii");
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return Buffer.concat([len, t, data, crc]);
}
function writePng(path) {
  const w = ex.fb_width(), h = ex.fb_height();
  const fb = new Uint8Array(ex.memory.buffer, ex.fb_ptr(), w * h);
  const raw = Buffer.alloc((w + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w + 1)] = 0; // filter: none
    for (let x = 0; x < w; x++) raw[y * (w + 1) + 1 + x] = Math.round((fb[y * w + x] * 255) / 15);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 0; // 8-bit grayscale
  const png = Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  writeFileSync(new URL(path, import.meta.url), png);
  console.log("wrote", path, `${w}x${h}`);
}

ex.bitcoin_init();
ex.bitcoin_tick();                       // idle -> draw dashboard
writePng("./shot-dashboard.png");

ex.bitcoin_start_get_pubkey(1);          // interactive xpub: draw the confirmation, then suspend
ex.bitcoin_poll();
writePng("./shot-confirm.png");

// Approve (tap the Next/Confirm location through the review), then capture "verified".
for (let i = 0; i < 8 && ex.bitcoin_poll() === -1n; i++) { ex.bitcoin_push_touch(399, 568, 1); ex.bitcoin_push_touch(399, 568, 0); }
writePng("./shot-verified.png");
