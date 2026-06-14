// Generic browser device shell for a Vanadium V-App built with the SDK's wasm-bindgen device
// exports (`vapp*`). App-independent: it paints the framebuffer to a <canvas>, pumps the
// device clock while idle (so the dashboard and its timers run), and routes canvas taps to
// the running command's screen or to the idle dashboard. The app-specific part — the command
// methods — lives entirely in the wasm-bindgen'd client; this shell never knows about it.

export class DeviceShell {
  // `api` exposes the SDK device shell: { memory, vappFbPtr, vappFbWidth, vappFbHeight,
  // vappFbVersion, vappTick, vappPushTouch, vappIdleTouch }.
  constructor(api, canvas) {
    this.api = api;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.busy = false;     // a client command is in flight
    this.stopped = false;  // the app exited / trapped
    this.lastTick = -Infinity;
    this.onstop = null;
    canvas.addEventListener("pointerdown", (e) => this._tap(e));
  }

  start() {
    requestAnimationFrame((t) => this._frame(t));
  }

  // Runs a command Promise (from the wasm-bindgen client), pausing the idle pump while it is
  // in flight so it doesn't draw the dashboard over the command's screen.
  async run(promise) {
    if (this.busy || this.stopped) return;
    this.busy = true;
    try {
      return await promise;
    } finally {
      this.busy = false;
    }
  }

  _frame(t) {
    if (this.stopped) return;
    try {
      // While idle, pump the device clock ~10x/s (draws the dashboard, advances its timers).
      if (!this.busy && t - this.lastTick > 100) {
        this.api.vappTick();
        this.lastTick = t;
      }
      this._paint();
    } catch (e) {
      this._stop(e);
      return;
    }
    requestAnimationFrame((tt) => this._frame(tt));
  }

  _paint() {
    const { memory, vappFbPtr, vappFbWidth, vappFbHeight } = this.api;
    const w = vappFbWidth(), h = vappFbHeight();
    if (this.canvas.width !== w) { this.canvas.width = w; this.canvas.height = h; }
    // Refetch the buffer each time: allocations can grow the memory and detach the old one.
    const fb = new Uint8Array(memory.buffer, vappFbPtr(), w * h);
    const img = this.ctx.createImageData(w, h);
    for (let i = 0; i < fb.length; i++) {
      const v = Math.round((fb[i] * 255) / 15); // Gray4 (0..15) -> 0..255
      img.data[i * 4] = v; img.data[i * 4 + 1] = v; img.data[i * 4 + 2] = v; img.data[i * 4 + 3] = 255;
    }
    this.ctx.putImageData(img, 0, 0);
  }

  _tap(e) {
    if (this.stopped) return;
    const r = this.canvas.getBoundingClientRect();
    const x = Math.floor((e.clientX - r.left) * this.canvas.width / r.width);
    const y = Math.floor((e.clientY - r.top) * this.canvas.height / r.height);
    try {
      if (this.busy) {
        // Feed the running command's screen (press + release).
        this.api.vappPushTouch(x, y, true);
        this.api.vappPushTouch(x, y, false);
      } else {
        this.api.vappIdleTouch(x, y); // dashboard app-info / quit navigation
        this._paint();
      }
    } catch (err) {
      this._stop(err); // e.g. tapping the dashboard's Quit calls the app's exit()
    }
  }

  _stop(e) {
    if (this.stopped) return;
    this.stopped = true;
    this.canvas.style.opacity = 0.35;
    if (this.onstop) this.onstop(e);
  }
}
