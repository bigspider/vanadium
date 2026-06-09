This is a minimal, self-contained **Snake** V-App for Vanadium.

It is a small demonstration of the low-level UI framework (`sdk::ui`) and raw button input.
The whole UI is drawn directly with the accelerated `fill_rect` primitive (no NBGL
pages/steps), and the app is fully standalone: it has **no client and does not communicate**
— it just runs its own menu and game loop on the device.

- [app](app) contains the RISC-V app for Vanadium. There is no client.

## Device support

The app is driven by the two physical buttons and the 128×64 screen, so it is built for the
**Nano S+** (and other two-button Nano devices). On a touch device it just shows a notice.

## How it works

On startup a small menu is shown with two entries, **Start** and **Exit**:

- **Left / Right button** — cycle the highlighted menu entry (handled on *release*).
- **Both buttons** — select the highlighted entry (on *release*).

Selecting **Start** plays a round of Snake:

- **Left button** — turn left (counter-clockwise).
- **Right button** — turn right (clockwise).
- **Both buttons** — abandon the round and return to the menu.

In the game the buttons are handled on *press* (not release) for an instant response. Eat
the pellets to grow and score; the round ends when the snake hits a wall or itself, the final
score is shown briefly, and the app returns to the menu. Selecting **Exit** quits the app.

## Build

From the `app` folder:

### RISC-V (Ledger Vanadium)

```sh
cargo build --release --target=riscv32imac-unknown-none-elf --no-default-features --features target_vanadium_ledger
```

### Native

```sh
cargo build
```

(`just build` builds both targets.)

## Package

To run on the Vanadium VM the V-App needs a manifest embedded in its binary. This is done
with [`cargo vnd`](../../cargo-vnd), which requires `riscv64-unknown-elf-objcopy` (on Ubuntu:
`sudo apt install binutils-riscv64-unknown-elf`) and the tool itself
(`cargo install --path ../../cargo-vnd` from this repo, or `cargo install --git
https://github.com/LedgerHQ/vanadium cargo-vnd`).

From the `app` folder, after the RISC-V build above:

```sh
cargo vnd package
```

This produces `target/riscv32imac-unknown-none-elf/release/vnd-snake.vapp` with the manifest
(from `[package.metadata.vapp]` in `Cargo.toml`) embedded.

## Run

The app runs inside the **Vanadium VM** (the Ledger app that hosts V-Apps). Since this app
is self-contained, you only need the generic `cargo vnd serve` to upload and start it — there
is no client to connect afterwards; you interact with the device buttons.

Because the app is built for the two-button Nano devices, use a **Nano S+** target.

1. Start the Vanadium VM on Speculos (in a separate terminal), from the [`vm`](../../vm)
   folder (the `nanosplus` VM binary must be built or downloaded — see `vm/download_vanadium.sh`):

   ```sh
   just run-nanosplus
   ```

2. Upload and start the packaged V-App:

   ```sh
   cargo vnd serve target/riscv32imac-unknown-none-elf/release/vnd-snake.vapp --speculos --no-hmacs
   ```

3. On the Speculos screen, approve the V-App registration prompt. The Snake menu then
   appears — drive everything with the buttons. Selecting **Exit** quits the V-App.

Leave `cargo vnd serve` running (it is the host-side bridge that keeps the VM event loop fed);
you never connect anything to the TCP port it opens. To run on a real device instead of
Speculos, use `--hid` in place of `--speculos`.

> If you skip packaging, you can serve the raw ELF instead
> (`cargo vnd serve target/riscv32imac-unknown-none-elf/release/vnd-snake --speculos --no-hmacs`).
> In that case `cargo vnd` reads the manifest from `Cargo.toml`, so run it from this `app`
> folder (or pass an absolute path to the ELF) so that lookup resolves.
