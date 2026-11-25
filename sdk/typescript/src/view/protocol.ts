export const MAX_TEXT_BYTES = 200_000;

const SERVER = { hello: 1, frame: 2, cursor: 3, pointer: 4, clipboard: 5 } as const;
const CLIENT = {
  ack: 1,
  pointer: 2,
  button: 3,
  key: 4,
  wheel: 5,
  pause: 6,
  symbol: 7,
  text: 8,
  clipboard: 9,
  copy: 10,
  chord: 11,
  pull: 12,
} as const;
const FRAME_HEADER_BYTES = 13;
const CURSOR_HEADER_BYTES = 9;
const MAX_CURSOR = 128;
const UNICODE_KEYSYM = 0x01000000;
const LATIN1_MAX_CODE_POINT = 0xff;

export type ServerMessage =
  | { type: "hello"; width: number; height: number; chords: boolean; watchOnly: boolean }
  | { type: "frame"; sequence: number; captureMicros: number; data: Uint8Array }
  | {
      type: "cursor";
      width: number;
      height: number;
      hotX: number;
      hotY: number;
      pixels: Uint8ClampedArray<ArrayBuffer>;
    }
  | { type: "pointer"; x: number; y: number }
  | { type: "clipboard"; text: string }
  | { type: "ignored" };

export type ClientMessage =
  | { type: "ack"; sequence: number; waitMs: number }
  | { type: "pointer"; x: number; y: number }
  | { type: "button"; button: number; down: boolean }
  | { type: "key"; code: number; down: boolean }
  | { type: "symbol"; symbol: number; down: boolean }
  | { type: "wheel"; dx: number; dy: number }
  | { type: "pause"; paused: boolean }
  | { type: "text"; text: string }
  | { type: "clipboard"; text: string }
  | { type: "copy" }
  | { type: "chord"; letter: "c" | "v" | "x"; shift: boolean }
  | { type: "pull" };

export function parseMessage(buffer: ArrayBuffer): ServerMessage {
  const view = new DataView(buffer);
  const type = buffer.byteLength > 0 ? view.getUint8(0) : 0;
  if (type === SERVER.hello) {
    if (buffer.byteLength < 5) throw new Error("The hello is too short");
    return {
      type: "hello",
      width: view.getUint16(1, true),
      height: view.getUint16(3, true),
      chords: buffer.byteLength >= 8 && (view.getUint8(7) & 1) === 1,
      watchOnly: buffer.byteLength >= 8 && (view.getUint8(7) & 2) === 2,
    };
  }
  if (type === SERVER.frame) {
    if (buffer.byteLength <= FRAME_HEADER_BYTES) throw new Error("The frame is too short");
    return {
      type: "frame",
      sequence: view.getUint32(1, true),
      captureMicros: Number(view.getBigUint64(5, true)),
      data: new Uint8Array(buffer, FRAME_HEADER_BYTES),
    };
  }
  if (type === SERVER.cursor) {
    if (buffer.byteLength < CURSOR_HEADER_BYTES) throw new Error("The cursor is too short");
    const width = view.getUint16(1, true);
    const height = view.getUint16(3, true);
    const end = CURSOR_HEADER_BYTES + width * height * 4;
    if (!width || !height || width > MAX_CURSOR || height > MAX_CURSOR || buffer.byteLength < end)
      return { type: "ignored" };
    return {
      type: "cursor",
      width,
      height,
      hotX: view.getUint16(5, true),
      hotY: view.getUint16(7, true),
      pixels: new Uint8ClampedArray(buffer.slice(CURSOR_HEADER_BYTES, end)),
    };
  }
  if (type === SERVER.pointer && buffer.byteLength >= 5)
    return { type: "pointer", x: view.getUint16(1, true), y: view.getUint16(3, true) };
  if (type === SERVER.clipboard)
    return { type: "clipboard", text: new TextDecoder().decode(new Uint8Array(buffer, 1)) };
  return { type: "ignored" };
}

function utf8(text: string): Uint8Array {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length <= MAX_TEXT_BYTES) return bytes;
  let end = MAX_TEXT_BYTES;
  while (end > 0 && ((bytes[end] ?? 0) & 0xc0) === 0x80) end--;
  return bytes.subarray(0, end);
}

function message(type: number, length: number): { buffer: ArrayBuffer; view: DataView } {
  const buffer = new ArrayBuffer(1 + length);
  const view = new DataView(buffer);
  view.setUint8(0, type);
  return { buffer, view };
}

function textMessage(type: number, text: string): ArrayBuffer {
  const bytes = utf8(text);
  const { buffer } = message(type, bytes.length);
  new Uint8Array(buffer).set(bytes, 1);
  return buffer;
}

function clampInt8(value: number): number {
  return Math.max(-10, Math.min(10, Math.trunc(value)));
}

export function encodeMessage(input: ClientMessage): ArrayBuffer {
  switch (input.type) {
    case "ack": {
      const { buffer, view } = message(CLIENT.ack, 8);
      view.setUint32(1, input.sequence, true);
      view.setUint32(5, Math.min(0xffffffff, Math.max(0, Math.round(input.waitMs * 1000))), true);
      return buffer;
    }
    case "pointer": {
      const { buffer, view } = message(CLIENT.pointer, 4);
      view.setUint16(1, input.x, true);
      view.setUint16(3, input.y, true);
      return buffer;
    }
    case "button": {
      const { buffer, view } = message(CLIENT.button, 2);
      view.setUint8(1, input.button);
      view.setUint8(2, input.down ? 1 : 0);
      return buffer;
    }
    case "key": {
      const { buffer, view } = message(CLIENT.key, 3);
      view.setUint16(1, input.code, true);
      view.setUint8(3, input.down ? 1 : 0);
      return buffer;
    }
    case "symbol": {
      const { buffer, view } = message(CLIENT.symbol, 5);
      view.setUint32(1, input.symbol, true);
      view.setUint8(5, input.down ? 1 : 0);
      return buffer;
    }
    case "wheel": {
      const { buffer, view } = message(CLIENT.wheel, 2);
      view.setInt8(1, clampInt8(input.dx));
      view.setInt8(2, clampInt8(input.dy));
      return buffer;
    }
    case "pause": {
      const { buffer, view } = message(CLIENT.pause, 1);
      view.setUint8(1, input.paused ? 1 : 0);
      return buffer;
    }
    case "text":
      return textMessage(CLIENT.text, input.text);
    case "clipboard":
      return textMessage(CLIENT.clipboard, input.text);
    case "copy":
      return message(CLIENT.copy, 0).buffer;
    case "chord": {
      const { buffer, view } = message(CLIENT.chord, 2);
      view.setUint8(1, input.letter.charCodeAt(0));
      view.setUint8(2, input.shift ? 1 : 0);
      return buffer;
    }
    case "pull":
      return message(CLIENT.pull, 0).buffer;
  }
}

export function characterKeysym(character: string): number {
  const codePoint = character.codePointAt(0) ?? 0;
  return codePoint <= LATIN1_MAX_CODE_POINT ? codePoint : UNICODE_KEYSYM + codePoint;
}

function sequence(first: number, names: string[]): [string, number][] {
  return names.map((name, index) => [name, first + index]);
}

const KEY_CODES = new Map<string, number>([
  ...sequence(1, [
    "Escape",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Digit0",
    "Minus",
    "Equal",
    "Backspace",
    "Tab",
    "KeyQ",
    "KeyW",
    "KeyE",
    "KeyR",
    "KeyT",
    "KeyY",
    "KeyU",
    "KeyI",
    "KeyO",
    "KeyP",
    "BracketLeft",
    "BracketRight",
    "Enter",
    "ControlLeft",
    "KeyA",
    "KeyS",
    "KeyD",
    "KeyF",
    "KeyG",
    "KeyH",
    "KeyJ",
    "KeyK",
    "KeyL",
    "Semicolon",
    "Quote",
    "Backquote",
    "ShiftLeft",
    "Backslash",
    "KeyZ",
    "KeyX",
    "KeyC",
    "KeyV",
    "KeyB",
    "KeyN",
    "KeyM",
    "Comma",
    "Period",
    "Slash",
    "ShiftRight",
    "NumpadMultiply",
    "AltLeft",
    "Space",
    "CapsLock",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "NumLock",
    "ScrollLock",
    "Numpad7",
    "Numpad8",
    "Numpad9",
    "NumpadSubtract",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "NumpadAdd",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad0",
    "NumpadDecimal",
  ]),
  ["IntlBackslash", 86],
  ["F11", 87],
  ["F12", 88],
  ["IntlRo", 89],
  ["Convert", 92],
  ["KanaMode", 93],
  ["NonConvert", 94],
  ...sequence(96, ["NumpadEnter", "ControlRight", "NumpadDivide", "PrintScreen", "AltRight"]),
  ...sequence(102, [
    "Home",
    "ArrowUp",
    "PageUp",
    "ArrowLeft",
    "ArrowRight",
    "End",
    "ArrowDown",
    "PageDown",
    "Insert",
    "Delete",
  ]),
  ["NumpadEqual", 117],
  ["Pause", 119],
  ["Lang1", 122],
  ["Lang2", 123],
  ["IntlYen", 124],
  ["MetaLeft", 125],
  ["MetaRight", 126],
  ["ContextMenu", 127],
  ...sequence(
    183,
    Array.from({ length: 12 }, (_, index) => `F${index + 13}`)
  ),
]);

export function keyCode(code: string): number | null {
  return KEY_CODES.get(code) ?? null;
}

export const MODIFIER_KEYS = [
  { state: "Shift", left: 42, right: 54 },
  { state: "Control", left: 29, right: 97 },
  { state: "Alt", left: 56, right: 100 },
  { state: "Meta", left: 125, right: 126 },
] as const;
