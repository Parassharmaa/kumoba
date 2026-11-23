import { connectViewer } from "./view.js";

const canvas = document.getElementById("screen");
const status = document.getElementById("status");
const keys = document.getElementById("keys");
const pointer = document.getElementById("pointer");
const pointerContext = pointer.getContext("2d");

const LOCAL_POINTER_MS = 600;
const MAX_ATTEMPTS = 6;
const PASTE_WAIT_MS = 120;
const PASTE_GAP_MS = 300;
const COPY_WINDOW_MS = 2000;
const APPLE = /Mac|iPhone|iPad/.test(navigator.platform);

const local = { x: 0, y: 0 };
const remote = { x: 0, y: 0, hotX: 0, hotY: 0, shape: false, localUntil: 0, timer: 0 };
let viewer = null;
let retry = 0;
let composing = false;
let pasteTimer = 0;
let pasteShift = false;
let pastedAt = 0;
let copyUntil = 0;

window.kumobaView = {
  get stats() {
    return viewer?.stats;
  },
};

function setStatus(text) {
  status.textContent = text;
}

function placePointer() {
  const box = canvas.getBoundingClientRect();
  const scale = box.width / canvas.width;
  pointer.hidden = !remote.shape || performance.now() < remote.localUntil;
  const x = box.left + (remote.x - remote.hotX) * scale;
  const y = box.top + (remote.y - remote.hotY) * scale;
  pointer.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
}

function placeKeys() {
  const x = Math.max(0, Math.min(local.x, innerWidth - 220));
  const y = Math.max(0, Math.min(local.y + 18, innerHeight - 40));
  keys.style.transform = `translate(${x}px, ${y}px)`;
}

function clipboardKey(event) {
  if (event.altKey) return "";
  if (event.key === "Insert") return event.shiftKey ? "v" : "";
  const command = APPLE ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  const letter = event.key.toLowerCase();
  return command && (letter === "c" || letter === "x" || letter === "v") ? letter : "";
}

function pasteWithoutEvent() {
  pasteTimer = 0;
  viewer?.paste("", pasteShift);
}

canvas.addEventListener("pointermove", (event) => {
  local.x = event.clientX;
  local.y = event.clientY;
  remote.localUntil = performance.now() + LOCAL_POINTER_MS;
  clearTimeout(remote.timer);
  remote.timer = setTimeout(placePointer, LOCAL_POINTER_MS + 20);
  placePointer();
});
canvas.addEventListener("pointerdown", () => keys.focus({ preventScroll: true }));
canvas.addEventListener("mousedown", (event) => event.preventDefault());

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
  if (clipboard && !event.repeat) copyUntil = performance.now() + COPY_WINDOW_MS;
  viewer?.key(event);
});
keys.addEventListener("keyup", (event) => viewer?.key(event));
keys.addEventListener("paste", (event) => {
  event.preventDefault();
  clearTimeout(pasteTimer);
  pasteTimer = 0;
  const now = performance.now();
  if (now - pastedAt < PASTE_GAP_MS) return;
  pastedAt = now;
  viewer?.paste(event.clipboardData?.getData("text/plain") ?? "", pasteShift);
});
keys.addEventListener("compositionstart", () => {
  composing = true;
  keys.classList.add("composing");
  placeKeys();
});
keys.addEventListener("compositionend", (event) => {
  composing = false;
  keys.classList.remove("composing");
  if (event.data) viewer?.typeText(event.data);
  setTimeout(() => {
    if (!composing) keys.value = "";
  }, 0);
});
keys.addEventListener("input", (event) => {
  if (composing || event.isComposing) return;
  if (event.inputType === "insertText" && event.data) viewer?.typeText(event.data);
  keys.value = "";
});
keys.addEventListener("blur", () => viewer?.releaseHeldInput());
addEventListener("resize", placePointer);

function connect() {
  const url = new URL("stream", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.search = location.search;
  url.hash = "";
  viewer = connectViewer(canvas, url, {
    onConnect: () => {
      retry = 0;
      setStatus("");
    },
    onClose: ({ error }) => {
      if (error) {
        setStatus(error.message);
        return;
      }
      retry += 1;
      if (retry > MAX_ATTEMPTS) {
        setStatus("The live view stopped. Run kumoba desktop again for a new link.");
        return;
      }
      setStatus("Reconnecting");
      setTimeout(connect, 250 * 2 ** retry);
    },
    onClipboard: (text) => {
      if (performance.now() >= copyUntil) return;
      copyUntil = 0;
      navigator.clipboard?.writeText(text).catch(() => {});
    },
    onCursor: ({ image, hotX, hotY }) => {
      remote.hotX = hotX;
      remote.hotY = hotY;
      remote.shape = true;
      pointer.width = image.width;
      pointer.height = image.height;
      pointerContext.putImageData(image, 0, 0);
      placePointer();
    },
    onPointer: (x, y) => {
      remote.x = x;
      remote.y = y;
      placePointer();
    },
  });
}

keys.focus({ preventScroll: true });
connect();
