import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { connectViewer, type ViewerOptions } from "../../src/view/index.js";

const fake = vi.hoisted(() => ({
  sockets: [] as FakeSocket[],
  frames: [] as (() => void)[],
  decodes: 0,
  failDecode: false,
}));

class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  readyState = FakeSocket.OPEN;
  binaryType = "blob";
  sent: number[][] = [];
  onmessage: ((event: { data: ArrayBuffer | string }) => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(public url: string) {
    fake.sockets.push(this);
  }
  send(data: ArrayBuffer) {
    this.sent.push([...new Uint8Array(data)]);
  }
  close() {
    this.readyState = FakeSocket.CLOSED;
  }
  receive(data: ArrayBuffer) {
    this.onmessage?.({ data });
  }
  drop() {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.();
  }
  of(type: number) {
    return this.sent.filter((message) => message[0] === type);
  }
  input() {
    return this.sent.filter((message) => message[0] !== 1 && message[0] !== 6);
  }
}

function fakeCodec() {
  const memory = new WebAssembly.Memory({ initial: 1 });
  return {
    memory,
    kv_open: () => 1,
    kv_input: () => 0,
    kv_decode: () => {
      fake.decodes += 1;
      return fake.failDecode ? 0 : 1;
    },
    kv_dirty: (index: number) => [0, 0, 4, 2][index] ?? 0,
    kv_pixels: () => 1024,
  };
}

function hello(width = 4, height = 2) {
  const view = new DataView(new ArrayBuffer(8));
  view.setUint8(0, 1);
  view.setUint16(1, width, true);
  view.setUint16(3, height, true);
  view.setUint16(5, 60, true);
  return view.buffer;
}

function frame(sequence: number, capturedMs = performance.now() - 100) {
  const view = new DataView(new ArrayBuffer(16));
  view.setUint8(0, 2);
  view.setUint32(1, sequence, true);
  view.setBigUint64(5, BigInt(Math.round(capturedMs * 1000)), true);
  return view.buffer;
}

function clipboard(text: string) {
  const bytes = new TextEncoder().encode(text);
  const buffer = new Uint8Array(bytes.length + 1);
  buffer[0] = 5;
  buffer.set(bytes, 1);
  return buffer.buffer;
}

function events() {
  return {
    onConnect: vi.fn(),
    onClose: vi.fn(),
    onClipboard: vi.fn(),
    onCursor: vi.fn(),
    onPointer: vi.fn(),
  };
}

function paint() {
  for (const callback of fake.frames.splice(0)) callback();
}

function acked(socket: FakeSocket) {
  return socket
    .of(1)
    .map((message) => new DataView(new Uint8Array(message).buffer).getUint32(1, true));
}

function key(type: "keydown" | "keyup", init: KeyboardEventInit) {
  return new KeyboardEvent(type, { cancelable: true, ...init });
}

function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  fake.sockets.length = 0;
  fake.frames.length = 0;
  fake.decodes = 0;
  fake.failDecode = false;
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.spyOn(WebAssembly, "instantiate").mockImplementation(async () => ({
    exports: fakeCodec(),
  }));
  vi.stubGlobal(
    "ImageData",
    class {
      constructor(
        public data: Uint8ClampedArray,
        public width: number,
        public height: number
      ) {}
    }
  );
  vi.stubGlobal("requestAnimationFrame", (callback: () => void) => {
    fake.frames.push(callback);
    return fake.frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function liveClient(handlers = events(), options: ViewerOptions = { viewOnly: true }) {
  const canvas = document.createElement("canvas");
  const client = connectViewer(canvas, "wss://gateway.example/stream", { ...options, ...handlers });
  const socket = fake.sockets.at(-1);
  if (!socket) throw new Error("No socket");
  socket.receive(hello());
  await vi.waitFor(() => expect(socket.of(6)).toHaveLength(1));
  return { client, socket, handlers, canvas };
}

async function liveControl(handlers = events()) {
  const live = await liveClient(handlers);
  live.socket.receive(frame(1));
  paint();
  live.client.setViewOnly(false);
  return live;
}

describe("viewer stream", () => {
  it("draws the first frame on an animation frame, then acknowledges it and connects", async () => {
    const { socket, handlers } = await liveClient();
    expect(fake.sockets[0]?.url).toBe("wss://gateway.example/stream");
    expect(socket.binaryType).toBe("arraybuffer");
    socket.receive(frame(1));
    expect(acked(socket)).toEqual([]);
    paint();
    expect(fake.decodes).toBe(1);
    expect(acked(socket)).toEqual([1]);
    expect(handlers.onConnect).toHaveBeenCalledOnce();
  });

  it("decodes messages that arrived before the decoder was ready", async () => {
    connectViewer(document.createElement("canvas"), "wss://gateway.example/stream", events());
    const [socket] = fake.sockets;
    if (!socket) throw new Error("No socket");
    socket.receive(hello());
    socket.receive(frame(1));
    await vi.mocked(WebAssembly.instantiate).mock.results.at(-1)?.value;
    await new Promise((resolve) => setTimeout(resolve, 0));
    paint();
    expect(acked(socket)).toEqual([1]);
  });

  it("draws a burst one frame per animation frame and drops only the latency-adding excess", async () => {
    const { socket } = await liveClient();
    for (const sequence of [1, 2, 3, 4]) socket.receive(frame(sequence));
    paint();
    expect(acked(socket)).toEqual([1, 2, 3]);
    paint();
    expect(acked(socket)).toEqual([1, 2, 3, 4]);
    expect(fake.decodes).toBe(4);
  });

  it("pauses while the tab is hidden and still acknowledges frames in flight", async () => {
    const { socket } = await liveClient();
    setHidden(true);
    expect(socket.of(6).at(-1)).toEqual([6, 1]);
    socket.receive(frame(1));
    expect(acked(socket)).toEqual([1]);
    setHidden(false);
    expect(socket.of(6).at(-1)).toEqual([6, 0]);
  });

  it("rejects the stream when no frame arrives in time", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const { handlers } = await liveClient(events(), { firstFrameTimeoutMs: 10_000 });
    expect(handlers.onClose).not.toHaveBeenCalled();
    vi.advanceTimersByTime(10_000);
    expect(handlers.onClose).toHaveBeenCalledWith({
      connected: false,
      error: new Error("The live view sent no frame in time"),
    });
  });

  it("reports a frame the decoder refuses as a lost connection", async () => {
    const { socket, handlers } = await liveControl();
    fake.failDecode = true;
    socket.receive(frame(2));
    paint();
    expect(handlers.onClose).toHaveBeenCalledWith({
      connected: true,
      error: new Error("The stream could not be decoded"),
    });
    expect(socket.readyState).toBe(FakeSocket.CLOSED);
  });

  it("tells a closed stream apart from one that never connected", async () => {
    const first = await liveClient();
    first.socket.drop();
    expect(first.handlers.onClose).toHaveBeenCalledWith({ connected: false });
    const second = await liveControl();
    second.socket.drop();
    expect(second.handlers.onClose).toHaveBeenCalledWith({ connected: true });
  });

  it("passes the remote clipboard on only while in control", async () => {
    const { socket, client, handlers } = await liveClient();
    socket.receive(clipboard("copied"));
    expect(handlers.onClipboard).not.toHaveBeenCalled();
    client.setViewOnly(false);
    socket.receive(clipboard("copied"));
    expect(handlers.onClipboard).toHaveBeenCalledWith("copied");
  });
});

describe("viewer input", () => {
  it("sends nothing while view-only", async () => {
    const { socket, client, canvas } = await liveClient();
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 1, clientY: 1 }));
    client.key(key("keydown", { key: "a", code: "KeyA" }));
    client.typeText("hi");
    client.paste("hi");
    expect(socket.input()).toEqual([]);
  });

  it("maps the pointer inside a letterboxed view to desktop pixels", async () => {
    const { socket, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 100 });
    canvas.setPointerCapture = vi.fn();
    canvas.dispatchEvent(new PointerEvent("pointerdown", { clientX: 200, clientY: 50, button: 0 }));
    canvas.dispatchEvent(new PointerEvent("pointerup", { clientX: 390, clientY: 99, button: 0 }));
    expect(socket.input()).toEqual([
      [2, 2, 0, 1, 0],
      [3, 1, 1],
      [2, 3, 0, 1, 0],
      [3, 1, 0],
    ]);
  });

  it("ignores a press in the margin around a letterboxed desktop", async () => {
    const { socket, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 100 });
    canvas.setPointerCapture = vi.fn();
    for (const clientX of [20, 99, 300, 380]) {
      canvas.dispatchEvent(new PointerEvent("pointerdown", { clientX, clientY: 50, button: 0 }));
      canvas.dispatchEvent(new PointerEvent("pointerup", { clientX, clientY: 50, button: 0 }));
    }
    expect(socket.input()).toEqual([]);
    expect(canvas.setPointerCapture).not.toHaveBeenCalled();
  });

  it("ignores pointer moves over the margin unless a button is held", async () => {
    const { socket, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 100 });
    canvas.setPointerCapture = vi.fn();
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 50, clientY: 50 }));
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 150, clientY: 50 }));
    canvas.dispatchEvent(new PointerEvent("pointerdown", { clientX: 150, clientY: 50, button: 0 }));
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 50, clientY: 50 }));
    canvas.dispatchEvent(new PointerEvent("pointerup", { clientX: 10, clientY: 50, button: 0 }));
    expect(socket.input()).toEqual([
      [2, 1, 0, 1, 0],
      [2, 1, 0, 1, 0],
      [3, 1, 1],
      [2, 0, 0, 1, 0],
      [2, 0, 0, 1, 0],
      [3, 1, 0],
    ]);
  });

  it("maps each screen pixel to the whole cell it covers", async () => {
    const { socket, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 400, height: 100 });
    canvas.setPointerCapture = vi.fn();
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 149, clientY: 99 }));
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: 299, clientY: 0 }));
    expect(socket.input()).toEqual([
      [2, 0, 0, 1, 0],
      [2, 3, 0, 0, 0],
    ]);
  });

  it("keeps touch drags for the desktop only while in control", async () => {
    const { client, canvas } = await liveClient();
    expect(canvas.style.touchAction).toBe("");
    client.setViewOnly(false);
    expect(canvas.style.touchAction).toBe("none");
    client.setViewOnly(true);
    expect(canvas.style.touchAction).toBe("");
  });

  it("lets go of a held button when the browser cancels the press", async () => {
    const { socket, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 4, height: 2 });
    canvas.setPointerCapture = vi.fn();
    canvas.dispatchEvent(new PointerEvent("pointerdown", { clientX: 1, clientY: 1, button: 0 }));
    canvas.dispatchEvent(new PointerEvent("pointercancel", { clientX: 1, clientY: 1, button: 0 }));
    canvas.dispatchEvent(new PointerEvent("pointerup", { clientX: 1, clientY: 1, button: 0 }));
    expect(socket.input()).toEqual([
      [2, 1, 0, 1, 0],
      [3, 1, 1],
      [3, 1, 0],
    ]);
  });

  it("scrolls by whole wheel notches and keeps the remainder", async () => {
    const { socket, canvas } = await liveControl();
    canvas.dispatchEvent(new WheelEvent("wheel", { deltaY: 60, cancelable: true }));
    canvas.dispatchEvent(new WheelEvent("wheel", { deltaY: 60, cancelable: true }));
    canvas.dispatchEvent(new WheelEvent("wheel", { deltaX: -300, cancelable: true }));
    expect(socket.input()).toEqual([
      [5, 0, 1],
      [5, 0xfd, 0],
    ]);
  });

  it("types printable keys as keysyms so the person's own layout decides the character", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "@", code: "BracketLeft" }));
    client.key(key("keyup", { key: "@", code: "BracketLeft" }));
    expect(socket.input()).toEqual([
      [7, 0x40, 0, 0, 0, 1],
      [7, 0x40, 0, 0, 0, 0],
    ]);
  });

  it("sends shortcuts as physical keys with their modifiers", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true }));
    client.key(key("keydown", { key: "a", code: "KeyA", ctrlKey: true }));
    client.key(key("keyup", { key: "a", code: "KeyA", ctrlKey: true }));
    client.key(key("keyup", { key: "Control", code: "ControlLeft" }));
    expect(socket.input()).toEqual([
      [4, 29, 0, 1],
      [4, 30, 0, 1],
      [4, 30, 0, 0],
      [4, 29, 0, 0],
    ]);
  });

  it("types an AltGr character without the held modifiers", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true }));
    client.key(key("keydown", { key: "€", code: "KeyE", ctrlKey: true, altKey: true }));
    expect(socket.input()).toEqual([
      [4, 29, 0, 1],
      [4, 29, 0, 0],
      [7, 0xac, 0x20, 0x00, 0x01, 1],
    ]);
  });

  it("swallows dead keys and repeats", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Dead", code: "Quote" }));
    client.key(key("keydown", { key: "a", code: "KeyA", repeat: true }));
    expect(socket.input()).toEqual([]);
  });

  it("releases a key the way it was pressed after its modifiers change", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "a", code: "KeyA" }));
    client.key(key("keyup", { key: "A", code: "KeyA", shiftKey: true }));
    expect(socket.input()).toEqual([
      [7, 0x61, 0, 0, 0, 1],
      [7, 0x61, 0, 0, 0, 0],
    ]);
  });

  it("asks for the copied text before a Ctrl+C reaches the desktop", async () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (X11; Linux x86_64)");
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true }));
    client.key(key("keydown", { key: "c", code: "KeyC", ctrlKey: true }));
    expect(socket.input()).toEqual([[4, 29, 0, 1], [10], [4, 46, 0, 1]]);
  });

  it("turns Cmd+C on a Mac into the desktop's copy chord", async () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh)");
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Meta", code: "MetaLeft", metaKey: true }));
    client.key(key("keydown", { key: "c", code: "KeyC", metaKey: true }));
    expect(socket.input()).toEqual([
      [4, 125, 0, 1],
      [4, 125, 0, 0],
      [11, 0x63, 0],
    ]);
  });

  it("pastes through the desktop clipboard plus the paste chord, modifiers lifted first", async () => {
    const { socket, client } = await liveControl();
    client.key(key("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true }));
    client.paste("東京\n千代田");
    const text = [...new TextEncoder().encode("東京\n千代田")];
    expect(socket.input()).toEqual([
      [4, 29, 0, 1],
      [4, 29, 0, 0],
      [9, ...text],
      [11, 0x76, 0],
    ]);
  });

  it("types committed IME text as text", async () => {
    const { socket, client } = await liveControl();
    client.typeText("日本");
    expect(socket.input()).toEqual([[8, ...new TextEncoder().encode("日本")]]);
  });

  it("releases held keys and buttons before going view-only", async () => {
    const { socket, client, canvas } = await liveControl();
    canvas.getBoundingClientRect = () => DOMRect.fromRect({ x: 0, y: 0, width: 4, height: 2 });
    canvas.setPointerCapture = vi.fn();
    client.key(key("keydown", { key: "Shift", code: "ShiftLeft", shiftKey: true }));
    client.key(key("keydown", { key: "A", code: "KeyA", shiftKey: true }));
    canvas.dispatchEvent(new PointerEvent("pointerdown", { clientX: 1, clientY: 1, button: 2 }));
    socket.sent.length = 0;
    client.releaseHeldInput();
    client.setViewOnly(true);
    expect(socket.input()).toEqual([
      [7, 0x41, 0, 0, 0, 0],
      [4, 42, 0, 0],
      [3, 3, 0],
    ]);
  });
});

describe("viewer page hooks", () => {
  it("hands the desktop's pointer shape and position to the page", async () => {
    const { socket, handlers, client, canvas } = await liveClient();
    const cursor = new DataView(new ArrayBuffer(9 + 4));
    cursor.setUint8(0, 3);
    cursor.setUint16(1, 1, true);
    cursor.setUint16(3, 1, true);
    cursor.setUint16(5, 0, true);
    cursor.setUint16(7, 0, true);
    socket.receive(cursor.buffer);
    socket.receive(new Uint8Array([4, 10, 0, 20, 0]).buffer);
    expect(handlers.onCursor).toHaveBeenCalledWith(expect.objectContaining({ hotX: 0, hotY: 0 }));
    expect(handlers.onPointer).toHaveBeenCalledWith(10, 20);
    expect(canvas.style.cursor).toBe("default");
    client.setViewOnly(false);
    expect(canvas.style.cursor).toContain("url(");
  });

  it("sends only the paste chord when the page has no text to paste", async () => {
    const { socket, client } = await liveControl();
    client.paste("", true);
    expect(socket.input()).toEqual([[11, 0x76, 1]]);
  });

  it("closes quietly when the page closes it", async () => {
    const { socket, client, handlers } = await liveControl();
    client.close();
    socket.receive(frame(2));
    paint();
    expect(socket.readyState).toBe(FakeSocket.CLOSED);
    expect(handlers.onClose).not.toHaveBeenCalled();
    expect(acked(socket)).toEqual([1]);
  });
});
