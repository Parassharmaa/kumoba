const canvas = document.getElementById("screen");
const status = document.getElementById("status");
const keys = document.getElementById("keys");
const context = canvas.getContext("2d", { alpha: false });

const SERVER_HELLO = 1;
const SERVER_FRAME = 2;
const SERVER_CURSOR = 3;
const SERVER_POINTER = 4;
const SERVER_CLIPBOARD = 5;
const CLIENT_ACK = 1;
const CLIENT_POINTER = 2;
const CLIENT_BUTTON = 3;
const CLIENT_KEY = 4;
const CLIENT_WHEEL = 5;

const CLIENT_PAUSE = 6;
const CLIENT_SYMBOL = 7;
const CLIENT_TEXT = 8;
const CLIENT_CLIPBOARD = 9;
const CLIENT_COPY = 10;
const CLIENT_CHORD = 11;

const FLOOR_DRIFT = 0.02;
const FLOOR_RESET_MS = 1000;
const MARGIN_LIMIT_MS = 60;
const STREAM_GAP_MS = 60;
const STREAM_LENGTH = 8;
const EARLY_MS = 4;
const MAX_WAITING = 2;
const LOCAL_POINTER_MS = 600;
const WHEEL_STEP = 100;
const MAX_CURSOR = 128;
const MAX_ATTEMPTS = 6;
const MAX_TEXT = 200000;
const PASTE_WAIT_MS = 120;
const PASTE_GAP_MS = 300;
const APPLE = /Mac|iPhone|iPad/.test(navigator.platform);
const COPY_WINDOW_MS = 2000;

const MODIFIERS = [
  ["Shift", 42, 54],
  ["Control", 29, 97],
  ["Alt", 56, 100],
  ["Meta", 125, 126],
];
const MODIFIER_CODES = new Set(MODIFIERS.flatMap(([, left, right]) => [left, right]));

const KEYS = {
  Escape: 1,
  Digit1: 2,
  Digit2: 3,
  Digit3: 4,
  Digit4: 5,
  Digit5: 6,
  Digit6: 7,
  Digit7: 8,
  Digit8: 9,
  Digit9: 10,
  Digit0: 11,
  Minus: 12,
  Equal: 13,
  Backspace: 14,
  Tab: 15,
  KeyQ: 16,
  KeyW: 17,
  KeyE: 18,
  KeyR: 19,
  KeyT: 20,
  KeyY: 21,
  KeyU: 22,
  KeyI: 23,
  KeyO: 24,
  KeyP: 25,
  BracketLeft: 26,
  BracketRight: 27,
  Enter: 28,
  ControlLeft: 29,
  KeyA: 30,
  KeyS: 31,
  KeyD: 32,
  KeyF: 33,
  KeyG: 34,
  KeyH: 35,
  KeyJ: 36,
  KeyK: 37,
  KeyL: 38,
  Semicolon: 39,
  Quote: 40,
  Backquote: 41,
  ShiftLeft: 42,
  Backslash: 43,
  KeyZ: 44,
  KeyX: 45,
  KeyC: 46,
  KeyV: 47,
  KeyB: 48,
  KeyN: 49,
  KeyM: 50,
  Comma: 51,
  Period: 52,
  Slash: 53,
  ShiftRight: 54,
  NumpadMultiply: 55,
  AltLeft: 56,
  Space: 57,
  CapsLock: 58,
  F1: 59,
  F2: 60,
  F3: 61,
  F4: 62,
  F5: 63,
  F6: 64,
  F7: 65,
  F8: 66,
  F9: 67,
  F10: 68,
  NumLock: 69,
  ScrollLock: 70,
  Numpad7: 71,
  Numpad8: 72,
  Numpad9: 73,
  NumpadSubtract: 74,
  Numpad4: 75,
  Numpad5: 76,
  Numpad6: 77,
  NumpadAdd: 78,
  Numpad1: 79,
  Numpad2: 80,
  Numpad3: 81,
  Numpad0: 82,
  NumpadDecimal: 83,
  IntlBackslash: 86,
  F11: 87,
  F12: 88,
  NumpadEnter: 96,
  ControlRight: 97,
  NumpadDivide: 98,
  PrintScreen: 99,
  AltRight: 100,
  Home: 102,
  ArrowUp: 103,
  PageUp: 104,
  ArrowLeft: 105,
  ArrowRight: 106,
  End: 107,
  ArrowDown: 108,
  PageDown: 109,
  Insert: 110,
  Delete: 111,
  Pause: 119,
  MetaLeft: 125,
  MetaRight: 126,
  ContextMenu: 127,
};

let codec = null;
let socket = null;
let image = null;
let imageBuffer = null;
let queue = [];
const playout = { floor: null, late: 0, spread: 0, streak: 0, lastCaptured: 0, lastArrived: 0 };
let frameHandle = 0;
let retry = 0;
const wheel = { x: 0, y: 0 };
const local = { x: 0, y: 0 };
let composing = false;
let pasteTimer = 0;
let pasteShift = false;
let pastedAt = 0;
let copyUntil = 0;
let stopped = false;
const pressed = new Map();
const modifiers = new Set();

export const stats = { received: 0, shown: 0, skipped: 0, bytes: 0, decodeMs: 0, marginMs: 0 };
window.kumobaView = { stats, playout, waiting: () => queue.length };

function setStatus(text) {
  status.textContent = text;
}

function resetPlayout() {
  playout.floor = null;
  playout.late = 0;
  playout.spread = 0;
  playout.streak = 0;
}

function margin() {
  if (playout.streak < STREAM_LENGTH) return 0;
  return Math.min(MARGIN_LIMIT_MS, playout.late + 2 * playout.spread);
}

function schedulePlayout(capturedMs, arrivedMs) {
  const delay = arrivedMs - capturedMs;
  if (playout.floor === null || Math.abs(delay - playout.floor) > FLOOR_RESET_MS) {
    resetPlayout();
    playout.floor = delay;
  } else {
    const drifted = playout.floor + (arrivedMs - playout.lastArrived) * FLOOR_DRIFT;
    playout.floor = Math.min(delay, drifted);
  }
  playout.streak = capturedMs - playout.lastCaptured < STREAM_GAP_MS ? playout.streak + 1 : 0;
  const late = delay - playout.floor;
  playout.spread += (Math.abs(late - playout.late) - playout.spread) / 4;
  playout.late += (late - playout.late) / 8;
  playout.lastCaptured = capturedMs;
  playout.lastArrived = arrivedMs;
  stats.marginMs = margin();
  return capturedMs + playout.floor;
}

function isDue(frame, now) {
  return document.hidden || frame.earliestMs + margin() <= now + EARLY_MS;
}

function decode(frame) {
  const started = performance.now();
  const pointer = codec.kv_input(frame.data.byteLength);
  new Uint8Array(codec.memory.buffer, pointer, frame.data.byteLength).set(frame.data);
  if (!codec.kv_decode()) throw new Error("The stream could not be decoded");
  stats.decodeMs += performance.now() - started;
  return [codec.kv_dirty(0), codec.kv_dirty(1), codec.kv_dirty(2), codec.kv_dirty(3)];
}

function blit(x, y, width, height) {
  if (imageBuffer !== codec.memory.buffer) {
    imageBuffer = codec.memory.buffer;
    const pixels = new Uint8ClampedArray(
      imageBuffer,
      codec.kv_pixels(),
      canvas.width * canvas.height * 4
    );
    image = new ImageData(pixels, canvas.width, canvas.height);
  }
  context.putImageData(image, 0, 0, x, y, width, height);
}

function acknowledge(frame, now) {
  if (socket?.readyState !== WebSocket.OPEN) return;
  const message = new DataView(new ArrayBuffer(9));
  message.setUint8(0, CLIENT_ACK);
  message.setUint32(1, frame.sequence, true);
  message.setUint32(
    5,
    Math.min(0xffffffff, Math.max(0, Math.round((now - frame.arrivedMs) * 1000))),
    true
  );
  socket.send(message);
}

function present() {
  frameHandle = 0;
  const now = performance.now();
  let left = Infinity, top = Infinity, right = 0, bottom = 0;
  const done = [];
  try {
    let due = 0;
    while (due < queue.length && isDue(queue[due], now)) due++;
    let take = document.hidden || due > MAX_WAITING ? due : Math.min(due, 1);
    if (!document.hidden && due > MAX_WAITING) take = due - 1;
    while (take-- > 0) {
      const frame = queue.shift();
      const [x, y, width, height] = decode(frame);
      if (width && height) {
        left = Math.min(left, x); top = Math.min(top, y);
        right = Math.max(right, x + width); bottom = Math.max(bottom, y + height);
      }
      done.push(frame);
    }
  } catch (error) {
    fail(error.message);
    return;
  }
  if (done.length) {
    if (right > left) blit(left, top, right - left, bottom - top);
    stats.shown++;
    stats.skipped += done.length - 1;
    for (const frame of done) acknowledge(frame, now);
    setStatus("");
  }
  if (queue.length) schedule();
}

function schedule() {
  if (document.hidden) present();
  else if (!frameHandle) frameHandle = requestAnimationFrame(present);
}

function reportVisibility() {
  send(new Uint8Array([CLIENT_PAUSE, document.hidden ? 1 : 0]));
  if (document.hidden && queue.length) {
    cancelAnimationFrame(frameHandle);
    present();
  }
}

function fail(message) {
  stopped = true;
  setStatus(message);
  socket?.close();
}

function send(bytes) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(bytes);
}

const pointer = document.getElementById('pointer');
const pointerContext = pointer.getContext('2d');
const remote = { x: 0, y: 0, hotX: 0, hotY: 0, shape: false, localUntil: 0, timer: 0 };

function placePointer() {
  const box = canvas.getBoundingClientRect();
  const scale = box.width / canvas.width;
  const local = performance.now() < remote.localUntil;
  pointer.hidden = !remote.shape || local;
  const x = box.left + (remote.x - remote.hotX) * scale;
  const y = box.top + (remote.y - remote.hotY) * scale;
  pointer.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
}

function showCursor(view, data) {
  const width = view.getUint16(1, true), height = view.getUint16(3, true);
  if (!width || !height || width > MAX_CURSOR || height > MAX_CURSOR) return;
  if (data.byteLength < 9 + width * height * 4) return;
  remote.hotX = view.getUint16(5, true);
  remote.hotY = view.getUint16(7, true);
  remote.shape = true;
  pointer.width = width;
  pointer.height = height;
  const pixels = new Uint8ClampedArray(data.slice(9, 9 + width * height * 4));
  pointerContext.putImageData(new ImageData(pixels, width, height), 0, 0);
  canvas.style.cursor = `url(${pointer.toDataURL()}) ${remote.hotX} ${remote.hotY}, default`;
  placePointer();
}

function position(event) {
  local.x = event.clientX;
  local.y = event.clientY;
  remote.localUntil = performance.now() + LOCAL_POINTER_MS;
  clearTimeout(remote.timer);
  remote.timer = setTimeout(placePointer, LOCAL_POINTER_MS + 20);
  placePointer();
  const box = canvas.getBoundingClientRect();
  const x = Math.round(((event.clientX - box.left) / box.width) * canvas.width);
  const y = Math.round(((event.clientY - box.top) / box.height) * canvas.height);
  const message = new DataView(new ArrayBuffer(5));
  message.setUint8(0, CLIENT_POINTER);
  message.setUint16(1, Math.max(0, Math.min(canvas.width - 1, x)), true);
  message.setUint16(3, Math.max(0, Math.min(canvas.height - 1, y)), true);
  send(message);
}

function button(event, down) {
  const code = [1, 2, 3, 8, 9][event.button];
  if (code) send(new Uint8Array([CLIENT_BUTTON, code, down ? 1 : 0]));
}

function key(code, down) {
  const message = new DataView(new ArrayBuffer(4));
  message.setUint8(0, CLIENT_KEY);
  message.setUint16(1, code, true);
  message.setUint8(3, down ? 1 : 0);
  send(message);
}

function symbol(value, down) {
  const message = new DataView(new ArrayBuffer(6));
  message.setUint8(0, CLIENT_SYMBOL);
  message.setUint32(1, value, true);
  message.setUint8(5, down ? 1 : 0);
  send(message);
}

function sendKey(entry, down) {
  if (entry.symbol) symbol(entry.symbol, down);
  else key(entry.code, down);
}

function entryFor(event) {
  const characters = [...event.key];
  if (characters.length === 1 && !event.altKey) {
    const point = characters[0].codePointAt(0);
    if (point > 0x20 && point !== 0x7f) {
      return { symbol: point <= 0xff ? point : 0x01000000 + point };
    }
  }
  const code = KEYS[event.code];
  return code ? { code } : null;
}

function syncModifiers(event) {
  const own = KEYS[event.code];
  for (const [name, left, right] of MODIFIERS) {
    if (own === left || own === right) continue;
    const wanted = event.getModifierState(name);
    const have = modifiers.has(left) || modifiers.has(right);
    if (wanted && !have) {
      modifiers.add(left);
      key(left, true);
    } else if (!wanted && have) {
      for (const code of [left, right]) {
        if (modifiers.delete(code)) key(code, false);
      }
    }
  }
}

function releaseKeys() {
  for (const entry of pressed.values()) {
    if (!modifiers.has(entry.code)) sendKey(entry, false);
  }
  pressed.clear();
  for (const code of modifiers) key(code, false);
  modifiers.clear();
}

canvas.addEventListener("pointermove", position);
canvas.addEventListener("pointerdown", (event) => {
  keys.focus({ preventScroll: true });
  canvas.setPointerCapture(event.pointerId);
  position(event);
  button(event, true);
  event.preventDefault();
});
canvas.addEventListener("pointerup", (event) => {
  position(event);
  button(event, false);
  event.preventDefault();
});
canvas.addEventListener("contextmenu", (event) => event.preventDefault());
canvas.addEventListener("mousedown", (event) => event.preventDefault());
canvas.addEventListener(
  "wheel",
  (event) => {
    event.preventDefault();
    const unit = event.deltaMode === WheelEvent.DOM_DELTA_PIXEL ? WHEEL_STEP : 1;
    wheel.x += event.deltaX / unit;
    wheel.y += event.deltaY / unit;
    const dx = Math.max(-10, Math.min(10, Math.trunc(wheel.x)));
    const dy = Math.max(-10, Math.min(10, Math.trunc(wheel.y)));
    if (!dx && !dy) return;
    wheel.x -= Math.trunc(wheel.x);
    wheel.y -= Math.trunc(wheel.y);
    send(new Int8Array([CLIENT_WHEEL, dx, dy]));
  },
  { passive: false }
);
function sendText(kind, value) {
  let bytes = new TextEncoder().encode(value);
  if (bytes.length > MAX_TEXT) {
    let end = MAX_TEXT;
    while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
    bytes = bytes.subarray(0, end);
  }
  const message = new Uint8Array(bytes.length + 1);
  message[0] = kind;
  message.set(bytes, 1);
  send(message);
}

function clipboardKey(event) {
  if (event.altKey) return "";
  if (event.key === "Insert") return event.shiftKey ? "v" : "";
  const command = APPLE ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  const letter = event.key.toLowerCase();
  return command && (letter === "c" || letter === "x" || letter === "v") ? letter : "";
}

function sendChord(letter, shift) {
  for (const code of modifiers) key(code, false);
  modifiers.clear();
  send(new Uint8Array([CLIENT_CHORD, letter.charCodeAt(0), shift ? 1 : 0]));
}

function pasteWithoutEvent() {
  pasteTimer = 0;
  sendChord("v", pasteShift);
}

function placeKeys() {
  const x = Math.max(0, Math.min(local.x, innerWidth - 220));
  const y = Math.max(0, Math.min(local.y + 18, innerHeight - 40));
  keys.style.transform = `translate(${x}px, ${y}px)`;
}

keys.addEventListener("keydown", (event) => {
  if (event.code === "CapsLock" || event.isComposing || event.keyCode === 229) return;
  const clipboard = clipboardKey(event);
  if (clipboard === "v") {
    if (event.repeat) return;
    pasteShift = event.shiftKey && event.key !== "Insert";
    clearTimeout(pasteTimer);
    pasteTimer = setTimeout(pasteWithoutEvent, PASTE_WAIT_MS);
    return;
  }
  event.preventDefault();
  if (event.repeat) return;
  if (clipboard && APPLE) {
    copyUntil = performance.now() + COPY_WINDOW_MS;
    sendChord(clipboard, event.shiftKey);
    return;
  }
  if (clipboard) {
    copyUntil = performance.now() + COPY_WINDOW_MS;
    send(new Uint8Array([CLIENT_COPY]));
  }
  syncModifiers(event);
  const entry = entryFor(event);
  if (!entry) return;
  pressed.set(event.code || event.key, entry);
  if (MODIFIER_CODES.has(entry.code)) modifiers.add(entry.code);
  sendKey(entry, true);
});
keys.addEventListener("keyup", (event) => {
  const id = event.code || event.key;
  const entry = pressed.get(id);
  if (!entry) return;
  event.preventDefault();
  pressed.delete(id);
  if (MODIFIER_CODES.has(entry.code)) {
    if (modifiers.delete(entry.code)) key(entry.code, false);
    if (entry.code === 125 || entry.code === 126) releaseKeys();
    return;
  }
  sendKey(entry, false);
});
keys.addEventListener("paste", (event) => {
  event.preventDefault();
  clearTimeout(pasteTimer);
  pasteTimer = 0;
  const now = performance.now();
  if (now - pastedAt < PASTE_GAP_MS) return;
  pastedAt = now;
  const value = event.clipboardData?.getData("text/plain") ?? "";
  if (value) sendText(CLIENT_CLIPBOARD, value);
  sendChord("v", pasteShift);
});
keys.addEventListener("compositionstart", () => {
  composing = true;
  keys.classList.add("composing");
  placeKeys();
});
keys.addEventListener("compositionend", (event) => {
  composing = false;
  keys.classList.remove("composing");
  if (event.data) sendText(CLIENT_TEXT, event.data);
  setTimeout(() => {
    if (!composing) keys.value = "";
  }, 0);
});
keys.addEventListener("input", (event) => {
  if (composing || event.isComposing) return;
  if (event.inputType === "insertText" && event.data) sendText(CLIENT_TEXT, event.data);
  keys.value = "";
});
keys.addEventListener("blur", releaseKeys);
document.addEventListener("visibilitychange", reportVisibility);
addEventListener("resize", placePointer);

function connect() {
  const url = new URL("stream", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.search = "";
  url.hash = "";
  const current = new WebSocket(url);
  current.binaryType = "arraybuffer";
  socket = current;
  queue = [];
  resetPlayout();
  current.onmessage = (event) => {
    if (socket !== current || !(event.data instanceof ArrayBuffer)) return;
    const view = new DataView(event.data);
    const type = view.byteLength ? view.getUint8(0) : 0;
    if (type === SERVER_HELLO && view.byteLength >= 5) {
      const width = view.getUint16(1, true);
      const height = view.getUint16(3, true);
      if (!codec.kv_open(width, height)) return fail("The desktop size is not supported");
      canvas.width = width;
      canvas.height = height;
      imageBuffer = null;
      retry = 0;
      reportVisibility();
    } else if (type === SERVER_CURSOR && view.byteLength >= 9) {
      showCursor(view, event.data);
    } else if (type === SERVER_CLIPBOARD) {
      if (performance.now() < copyUntil) {
        copyUntil = 0;
        const value = new TextDecoder().decode(new Uint8Array(event.data, 1));
        navigator.clipboard?.writeText(value).catch(() => {});
      }
    } else if (type === SERVER_POINTER && view.byteLength >= 5) {
      remote.x = view.getUint16(1, true);
      remote.y = view.getUint16(3, true);
      placePointer();
    } else if (type === SERVER_FRAME && view.byteLength > 13) {
      const arrivedMs = performance.now();
      const capturedMs = Number(view.getBigUint64(5, true)) / 1000;
      stats.received++;
      stats.bytes += event.data.byteLength;
      queue.push({
        sequence: view.getUint32(1, true),
        capturedMs,
        arrivedMs,
        earliestMs: schedulePlayout(capturedMs, arrivedMs),
        data: new Uint8Array(event.data, 13),
      });
      schedule();
    }
  };
  current.onclose = () => {
    if (socket !== current) return;
    releaseKeys();
    if (stopped) return;
    retry += 1;
    if (retry > MAX_ATTEMPTS) {
      setStatus("The live view stopped. Run kumoba desktop again for a new link.");
      return;
    }
    setStatus("Reconnecting");
    setTimeout(connect, 250 * 2 ** retry);
  };
}

const module = await WebAssembly.instantiate(await (await fetch("codec.wasm")).arrayBuffer(), {});
codec = module.instance.exports;
keys.focus({ preventScroll: true });
connect();
