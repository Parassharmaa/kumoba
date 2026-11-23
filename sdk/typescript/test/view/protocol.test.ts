import { describe, expect, it } from "vitest";
import { createDecoder } from "../../src/view/decoder.js";
import {
  characterKeysym,
  encodeMessage,
  keyCode,
  MAX_TEXT_BYTES,
  parseMessage,
} from "../../src/view/protocol.js";

function bytes(...values: number[]) {
  return new Uint8Array(values).buffer;
}

function encoded(input: Parameters<typeof encodeMessage>[0]) {
  return [...new Uint8Array(encodeMessage(input))];
}

describe("parseMessage", () => {
  it("reads the hello with the desktop size and chord flag", () => {
    expect(parseMessage(bytes(1, 0x00, 0x04, 0x00, 0x03, 60, 0, 1))).toEqual({
      type: "hello",
      width: 1024,
      height: 768,
      chords: true,
    });
    expect(parseMessage(bytes(1, 0x00, 0x04, 0x00, 0x03))).toMatchObject({ chords: false });
  });

  it("reads a frame's sequence, capture time and payload", () => {
    const buffer = new ArrayBuffer(16);
    const view = new DataView(buffer);
    view.setUint8(0, 2);
    view.setUint32(1, 42, true);
    view.setBigUint64(5, 1_500_000n, true);
    new Uint8Array(buffer, 13).set([7, 8, 9]);
    const message = parseMessage(buffer);
    expect(message).toMatchObject({ type: "frame", sequence: 42, captureMicros: 1_500_000 });
    expect(message.type === "frame" && [...message.data]).toEqual([7, 8, 9]);
  });

  it("refuses a frame or hello that is too short", () => {
    expect(() => parseMessage(bytes(2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))).toThrow();
    expect(() => parseMessage(bytes(1, 0, 4))).toThrow();
  });

  it("reads a cursor shape and ignores one with an impossible size", () => {
    const cursor = new ArrayBuffer(9 + 2 * 1 * 4);
    const view = new DataView(cursor);
    view.setUint8(0, 3);
    view.setUint16(1, 2, true);
    view.setUint16(3, 1, true);
    view.setUint16(5, 1, true);
    view.setUint16(7, 0, true);
    new Uint8Array(cursor, 9).set([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(parseMessage(cursor)).toMatchObject({
      type: "cursor",
      width: 2,
      height: 1,
      hotX: 1,
      hotY: 0,
      pixels: new Uint8ClampedArray([1, 2, 3, 4, 5, 6, 7, 8]),
    });
    view.setUint16(1, 200, true);
    expect(parseMessage(cursor)).toEqual({ type: "ignored" });
  });

  it("reads clipboard text as UTF-8", () => {
    const text = new TextEncoder().encode("東京\n");
    const buffer = new Uint8Array(text.length + 1);
    buffer[0] = 5;
    buffer.set(text, 1);
    expect(parseMessage(buffer.buffer)).toEqual({ type: "clipboard", text: "東京\n" });
  });

  it("reads the desktop pointer position and ignores unknown messages", () => {
    expect(parseMessage(bytes(4, 1, 0, 2, 1))).toEqual({ type: "pointer", x: 1, y: 258 });
    expect(parseMessage(bytes(99))).toEqual({ type: "ignored" });
  });
});

describe("encodeMessage", () => {
  it("encodes an ACK with the time the frame waited in microseconds", () => {
    expect(encoded({ type: "ack", sequence: 258, waitMs: 1.5 })).toEqual([
      1, 2, 1, 0, 0, 0xdc, 0x05, 0, 0,
    ]);
  });

  it("encodes pointer, button, key, symbol, wheel and pause input", () => {
    expect(encoded({ type: "pointer", x: 1023, y: 2 })).toEqual([2, 0xff, 0x03, 2, 0]);
    expect(encoded({ type: "button", button: 3, down: true })).toEqual([3, 3, 1]);
    expect(encoded({ type: "key", code: 29, down: false })).toEqual([4, 29, 0, 0]);
    expect(encoded({ type: "symbol", symbol: 0x01006771, down: true })).toEqual([
      7, 0x71, 0x67, 0x00, 0x01, 1,
    ]);
    expect(encoded({ type: "wheel", dx: -30, dy: 2 })).toEqual([5, 0xf6, 2]);
    expect(encoded({ type: "pause", paused: true })).toEqual([6, 1]);
  });

  it("encodes text, clipboard, copy and chords", () => {
    expect(encoded({ type: "text", text: "é" })).toEqual([8, 0xc3, 0xa9]);
    expect(encoded({ type: "clipboard", text: "a\nb" })).toEqual([9, 0x61, 0x0a, 0x62]);
    expect(encoded({ type: "copy" })).toEqual([10]);
    expect(encoded({ type: "chord", letter: "v", shift: true })).toEqual([11, 0x76, 1]);
  });

  it("cuts long text on a character boundary", () => {
    const text = "あ".repeat(MAX_TEXT_BYTES);
    const message = new Uint8Array(encodeMessage({ type: "clipboard", text }));
    expect(message.length - 1).toBeLessThanOrEqual(MAX_TEXT_BYTES);
    expect((message.length - 1) % 3).toBe(0);
    expect(new TextDecoder("utf-8", { fatal: true }).decode(message.subarray(1))).toMatch(/^あ+$/);
  });
});

describe("keys", () => {
  it("maps physical keys to Linux key codes, JIS keys included", () => {
    expect(keyCode("KeyA")).toBe(30);
    expect(keyCode("Enter")).toBe(28);
    expect(keyCode("ControlRight")).toBe(97);
    expect(keyCode("ArrowLeft")).toBe(105);
    expect(keyCode("IntlYen")).toBe(124);
    expect(keyCode("F24")).toBe(194);
    expect(keyCode("Unidentified")).toBeNull();
  });

  it("sends Latin-1 characters as their keysym and others as Unicode keysyms", () => {
    expect(characterKeysym("@")).toBe(0x40);
    expect(characterKeysym("é")).toBe(0xe9);
    expect(characterKeysym("€")).toBe(0x010020ac);
  });
});

describe("decoder", () => {
  it("instantiates the decoder bundled in the package", async () => {
    const decoder = await createDecoder();
    expect(decoder.kv_open(64, 32)).toBe(1);
    expect(decoder.memory).toBeInstanceOf(WebAssembly.Memory);
  });
});
