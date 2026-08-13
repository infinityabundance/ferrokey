// Ferrokey court target: a minimal Electron application.
//
// Electron must be its own compatibility court (rule 53): Chromium-in-a-shell
// behaves differently from a browser under the same input stack. This app
// reports, over the standard target reporter socket:
//
//   {"event":"ready"}                      on connect
//   {"event":"focus","focused":bool}       window focus in/out
//   {"event":"key","code":"KeyA","down":bool}  raw key transitions (DOM code)
//   {"event":"text","text":"..."}          the input field's value
//
// Run (in the court VM): /opt/electron/electron <app-dir> --no-sandbox
"use strict";

const { app, BrowserWindow, ipcMain } = require("electron");
const net = require("net");

const SOCKET = process.env.TARGET_SOCKET || "/tmp/ferrokey-test-target.sock";

// Diagnose main-process failures instead of popping Electron's "Error"
// dialog window (which would steal focus and break the court).
process.on("uncaughtException", (err) => {
  console.error("UNCAUGHT:", err && err.stack ? err.stack : err);
});
process.on("unhandledRejection", (reason) => {
  console.error("UNHANDLED_REJECTION:", reason);
});

class Reporter {
  // Bind the report socket like the Rust targets do (ferrokey-test-common
  // Reporter::bind): this app is the SERVER; the court's recv-events.py
  // connects as a client. A net.connect client here would find no listener
  // and the court could never receive events.
  //
  // New clients get a state snapshot (ready + current focus/text), mirroring
  // the Rust reporter: the court's recorder may connect at any point and must
  // not miss events that already happened.
  constructor(path) {
    try { require("fs").unlinkSync(path); } catch (_) {}
    this.server = net.createServer((sock) => {
      sock.on("error", () => {});
      this.sock = sock;
      if (this.appReady) {
        this.send({ event: "ready" });
        if (this._focused !== null) this.focus(this._focused);
        if (this._text !== null) this.text(this._text);
      }
    });
    this.server.listen(path);
    this.server.on("error", () => {});
    this.sock = null;
    this.appReady = false;
    this._focused = null;
    this._text = null;
  }
  send(obj) {
    try {
      if (!this.sock) return;
      this.sock.write(JSON.stringify(obj) + "\n");
    } catch (_) {}
  }
  ready() {
    this.appReady = true;
    this.send({ event: "ready" });
  }
  focus(focused) {
    this._focused = focused;
    this.send({ event: "focus", focused });
  }
  key(code, down) { this.send({ event: "key", code, down }); }
  text(text) {
    this._text = text;
    this.send({ event: "text", text });
  }
}

let reporter = null;

app.whenReady().then(() => {
  reporter = new Reporter(SOCKET);
  reporter.ready();

  const win = new BrowserWindow({
    width: 420,
    height: 120,
    x: 100,
    y: 480, // below the OSK (full view is 1160x460 at (0,0))
    title: "ferrokey-test-target-electron",
    webPreferences: { nodeIntegration: true, contextIsolation: false },
  });
  win.loadFile("index.html");

  win.on("focus", () => reporter.focus(true));
  win.on("blur", () => reporter.focus(false));

  // Raw key transitions, before the page sees them (DOM code strings).
  win.webContents.on("before-input-event", (event, input) => {
    if (input.type === "keyDown" || input.type === "keyUp") {
      reporter.key(input.code, input.type === "keyDown");
    }
    if (input.type === "keyDown" && input.key === "Escape") {
      app.quit();
    }
  });

  ipcMain.on("text", (_event, value) => reporter.text(value));
});
