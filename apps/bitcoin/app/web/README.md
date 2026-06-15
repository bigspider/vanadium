# Bitcoin V-App in the browser

The real Bitcoin V-App **and** the real Rust `BitcoinClient`, compiled to one wasm module and
exposed to JS with [wasm-bindgen]. There is no separate demo crate — this is built straight
from the `vnd-bitcoin` app crate (`src/wasm.rs`), and the generic device shell
(framebuffer/input/dashboard) comes from the SDK.

## Build

```sh
cargo install wasm-bindgen-cli --version 0.2.105   # must match the wasm-bindgen crate
./build.sh            # debug   -> pkg/ (browser) + pkg-node/ (node test)
./build.sh --release  # smaller; recommended for a hosted demo
```

`pkg/` and `pkg-node/` are generated (gitignored).

## Run

Page (serve over http — a `file://` page can't fetch the `.wasm`):

```sh
python3 -m http.server   # from apps/bitcoin/app, then open http://localhost:8000/web/
```

Headless test (the real client driving the real app):

```sh
node test.mjs
```

## How it works

- `new BitcoinApp()` installs the V-App into the page's device; the client's methods are JS
  Promises: `await app.getExtendedPubkey("m/84'/1'/0'", true)` runs the on-device confirmation
  and resolves to the xpub (or rejects with the typed error).
- `device-shell.mjs` is app-independent: it paints the framebuffer, pumps the dashboard while
  idle, and routes taps. Reusable by any V-App built with the SDK's `vapp*` exports.

## Adding a command

Add a method to `BitcoinApp` in `../src/wasm.rs` that calls the corresponding real
`BitcoinClient` method and wraps it in `future_to_promise`. That's the only app-specific code —
the wasm ABI and the device shell are generic.

[wasm-bindgen]: https://rustwasm.github.io/wasm-bindgen/
