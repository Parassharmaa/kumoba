import { createDecoder, type Decoder } from "./decoder.js";
import {
  type ClientMessage,
  characterKeysym,
  encodeMessage,
  keyCode,
  MODIFIER_KEYS,
  parseMessage,
} from "./protocol.js";

export type ViewerCursor = { image: ImageData; hotX: number; hotY: number };

export type ViewerClose = { connected: boolean; error?: Error };

export type ViewerOptions = {
  viewOnly?: boolean;
  firstFrameTimeoutMs?: number;
  onConnect?: () => void;
  onClose?: (close: ViewerClose) => void;
  onClipboard?: (text: string) => void;
  onCursor?: (cursor: ViewerCursor) => void;
  onPointer?: (x: number, y: number) => void;
};

export type ViewerStats = {
  received: number;
  shown: number;
  skipped: number;
  bytes: number;
  decodeMs: number;
  marginMs: number;
  waiting: number;
};

export type Viewer = {
  readonly stats: ViewerStats;
  viewOnly(): boolean;
  setViewOnly(viewOnly: boolean): void;
  key(event: KeyboardEvent): void;
  typeText(text: string): void;
  paste(text: string, shift?: boolean): void;
  releaseHeldInput(): void;
  close(): void;
};

const POINTER_BUTTONS = [1, 2, 3, 8, 9];
const BUTTON_BITS = [1, 4, 2, 8, 16];
const WHEEL_LINES_PER_NOTCH = 3;
const WHEEL_PIXELS_PER_NOTCH = 100;
const WHEEL_STEP = 120;
const MAX_WAITING_FRAMES = 2;
const FLOOR_DRIFT = 0.02;
const FLOOR_RESET_MS = 1000;
const MARGIN_LIMIT_MS = 60;
const STREAM_GAP_MS = 60;
const STREAM_LENGTH = 8;
const EARLY_MS = 4;
const FIRST_PRINTABLE_CODE_POINT = 0x20;
const SHORTCUT_CHARACTER = /^[A-Za-z0-9]$/;
const APPLE_PLATFORM = /(Mac|iPhone|iPod|iPad)/i;
const META_CODES = new Set<number>([125, 126]);
const MODIFIER_CODES = new Set<number>(MODIFIER_KEYS.flatMap(({ left, right }) => [left, right]));

type LegacyWheelEvent = WheelEvent & { wheelDeltaX?: number; wheelDeltaY?: number };
type KeyEntry = { kind: "code"; code: number } | { kind: "symbol"; symbol: number };
type QueuedFrame = { sequence: number; arrived: number; earliest: number; data: Uint8Array };

function wheelNotches(event: LegacyWheelEvent, axis: "vertical" | "horizontal"): number {
  const legacy = axis === "vertical" ? event.wheelDeltaY : event.wheelDeltaX;
  if (typeof legacy === "number" && legacy !== 0) return -legacy / WHEEL_STEP;
  const delta = axis === "vertical" ? event.deltaY : event.deltaX;
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) return delta / WHEEL_LINES_PER_NOTCH;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) return delta;
  return delta / WHEEL_PIXELS_PER_NOTCH;
}

function isTypedCharacter(event: KeyboardEvent): boolean {
  if (event.metaKey || Array.from(event.key).length !== 1) return false;
  if ((event.key.codePointAt(0) ?? 0) < FIRST_PRINTABLE_CODE_POINT) return false;
  if (event.ctrlKey && !event.altKey) return false;
  if (event.ctrlKey || event.altKey) return !SHORTCUT_CHARACTER.test(event.key);
  return true;
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

export function connectViewer(
  canvas: HTMLCanvasElement,
  url: string | URL,
  options: ViewerOptions = {}
): Viewer {
  const context = canvas.getContext("2d", { alpha: false });
  const socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  const apple = APPLE_PLATFORM.test(navigator.userAgent);
  const stats: ViewerStats = {
    received: 0,
    shown: 0,
    skipped: 0,
    bytes: 0,
    decodeMs: 0,
    marginMs: 0,
    waiting: 0,
  };
  let chords = apple;
  let viewOnly = options.viewOnly ?? false;
  let closed = false;
  let connected = false;
  let decoder: Decoder | null = null;
  let image: ImageData | null = null;
  let imageBuffer: ArrayBufferLike | null = null;
  let remoteCursor = "default";
  let animation = 0;
  let queue: QueuedFrame[] = [];
  const early: ArrayBuffer[] = [];
  const playout = { floor: null as number | null, late: 0, spread: 0, streak: 0 };
  const last = { captured: 0, arrived: 0 };
  const pressed = new Map<string, KeyEntry>();
  const modifiers = new Set<number>();
  const buttons = new Set<number>();
  const wheelRemainder = { vertical: 0, horizontal: 0 };

  const pageTouchAction = canvas.style.touchAction;
  const applyCursor = () => {
    canvas.style.cursor = viewOnly ? "default" : remoteCursor;
    canvas.style.touchAction = viewOnly ? pageTouchAction : "none";
  };
  applyCursor();

  const transmit = (message: ClientMessage) => {
    if (!closed && socket.readyState === WebSocket.OPEN) socket.send(encodeMessage(message));
  };
  const send = (message: ClientMessage) => {
    if (!viewOnly) transmit(message);
  };

  const margin = () =>
    playout.streak < STREAM_LENGTH
      ? 0
      : Math.min(MARGIN_LIMIT_MS, playout.late + 2 * playout.spread);

  const schedulePlayout = (captured: number, arrived: number) => {
    const delay = arrived - captured;
    if (playout.floor === null || Math.abs(delay - playout.floor) > FLOOR_RESET_MS) {
      Object.assign(playout, { floor: delay, late: 0, spread: 0, streak: 0 });
    } else {
      playout.floor = Math.min(delay, playout.floor + (arrived - last.arrived) * FLOOR_DRIFT);
    }
    const floor = playout.floor ?? delay;
    playout.streak = captured - last.captured < STREAM_GAP_MS ? playout.streak + 1 : 0;
    const late = delay - floor;
    playout.spread += (Math.abs(late - playout.late) - playout.spread) / 4;
    playout.late += (late - playout.late) / 8;
    last.captured = captured;
    last.arrived = arrived;
    stats.marginMs = margin();
    return captured + floor;
  };

  const blit = (codec: Decoder, x: number, y: number, width: number, height: number) => {
    if (!image || imageBuffer !== codec.memory.buffer) {
      imageBuffer = codec.memory.buffer;
      image = new ImageData(
        new Uint8ClampedArray(imageBuffer, codec.kv_pixels(), canvas.width * canvas.height * 4),
        canvas.width,
        canvas.height
      );
    }
    context?.putImageData(image, 0, 0, x, y, width, height);
  };

  const decode = (codec: Decoder, frame: QueuedFrame) => {
    const started = performance.now();
    const pointer = codec.kv_input(frame.data.byteLength);
    new Uint8Array(codec.memory.buffer, pointer, frame.data.byteLength).set(frame.data);
    if (!codec.kv_decode()) throw new Error("The stream could not be decoded");
    stats.decodeMs += performance.now() - started;
    return [0, 1, 2, 3].map((index) => codec.kv_dirty(index));
  };

  const end = (error?: Error) => {
    if (closed) return;
    const wasConnected = connected;
    shutdown();
    options.onClose?.(error ? { connected: wasConnected, error } : { connected: wasConnected });
  };

  const present = () => {
    animation = 0;
    if (closed || !decoder) return;
    const now = performance.now();
    const hidden = document.hidden;
    let due = 0;
    while (
      due < queue.length &&
      (hidden || (queue[due]?.earliest ?? 0) + margin() <= now + EARLY_MS)
    )
      due++;
    let take = Math.min(due, 1);
    if (hidden) take = due;
    else if (due > MAX_WAITING_FRAMES) take = due - 1;
    const done = queue.splice(0, take);
    stats.waiting = queue.length;
    let left = Number.POSITIVE_INFINITY;
    let top = Number.POSITIVE_INFINITY;
    let right = 0;
    let bottom = 0;
    try {
      for (const frame of done) {
        const [x = 0, y = 0, width = 0, height = 0] = decode(decoder, frame);
        if (!width || !height) continue;
        left = Math.min(left, x);
        top = Math.min(top, y);
        right = Math.max(right, x + width);
        bottom = Math.max(bottom, y + height);
      }
    } catch (error: unknown) {
      end(asError(error));
      return;
    }
    if (done.length > 0) {
      if (right > left) blit(decoder, left, top, right - left, bottom - top);
      stats.shown++;
      stats.skipped += done.length - 1;
      for (const frame of done)
        transmit({ type: "ack", sequence: frame.sequence, waitMs: now - frame.arrived });
      if (!connected) {
        connected = true;
        clearTimeout(firstFrameTimeout);
        options.onConnect?.();
      }
    }
    if (queue.length > 0) schedule();
  };

  const schedule = () => {
    if (document.hidden) present();
    else animation ||= requestAnimationFrame(present);
  };

  const onVisibility = () => {
    transmit({ type: "pause", paused: document.hidden });
    if (document.hidden && queue.length > 0) {
      cancelAnimationFrame(animation);
      present();
    }
  };

  const firstFrameTimeout =
    options.firstFrameTimeoutMs === undefined
      ? undefined
      : setTimeout(() => {
          if (!connected) end(new Error("The live view sent no frame in time"));
        }, options.firstFrameTimeoutMs);

  const handle = (codec: Decoder, data: ArrayBuffer) => {
    const message = parseMessage(data);
    if (message.type === "hello") {
      if (!codec.kv_open(message.width, message.height))
        throw new Error("The desktop size is not supported");
      canvas.width = message.width;
      canvas.height = message.height;
      image = null;
      chords = apple || message.chords;
      transmit({ type: "pause", paused: document.hidden });
      return;
    }
    if (message.type === "frame") {
      const arrived = performance.now();
      stats.received++;
      stats.bytes += data.byteLength;
      queue.push({
        sequence: message.sequence,
        arrived,
        earliest: schedulePlayout(message.captureMicros / 1000, arrived),
        data: message.data,
      });
      stats.waiting = queue.length;
      schedule();
      return;
    }
    if (message.type === "cursor") {
      const image = new ImageData(message.pixels, message.width, message.height);
      const shape = document.createElement("canvas");
      shape.width = message.width;
      shape.height = message.height;
      shape.getContext("2d")?.putImageData(image, 0, 0);
      remoteCursor = `url(${shape.toDataURL()}) ${message.hotX} ${message.hotY}, default`;
      applyCursor();
      options.onCursor?.({ image, hotX: message.hotX, hotY: message.hotY });
      return;
    }
    if (message.type === "pointer") {
      options.onPointer?.(message.x, message.y);
      return;
    }
    if (message.type === "clipboard" && !viewOnly) options.onClipboard?.(message.text);
  };

  socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
    if (closed || typeof event.data === "string") return;
    if (!decoder) {
      early.push(event.data);
      return;
    }
    try {
      handle(decoder, event.data);
    } catch (error: unknown) {
      end(asError(error));
    }
  };
  socket.onclose = () => end();
  createDecoder().then(
    (codec) => {
      if (closed) return;
      decoder = codec;
      try {
        for (const data of early.splice(0)) handle(codec, data);
      } catch (error: unknown) {
        end(asError(error));
      }
    },
    (error: unknown) => end(asError(error))
  );
  document.addEventListener("visibilitychange", onVisibility);

  const locate = (event: MouseEvent) => {
    const bounds = canvas.getBoundingClientRect();
    const width = canvas.width || 1;
    const height = canvas.height || 1;
    const scale = Math.min(bounds.width / width, bounds.height / height) || 1;
    const left = bounds.left + (bounds.width - width * scale) / 2;
    const top = bounds.top + (bounds.height - height * scale) / 2;
    const x = Math.floor((event.clientX - left) / scale);
    const y = Math.floor((event.clientY - top) / scale);
    return {
      inside: x >= 0 && x < width && y >= 0 && y < height,
      x: Math.max(0, Math.min(width - 1, x)),
      y: Math.max(0, Math.min(height - 1, y)),
    };
  };
  const moveTo = ({ x, y }: { x: number; y: number }) => send({ type: "pointer", x, y });
  const onPointerMove = (event: PointerEvent) => {
    const place = locate(event);
    const chord = POINTER_BUTTONS[event.button];
    if (chord && buttons.size > 0) {
      const down = (event.buttons & (BUTTON_BITS[event.button] ?? 0)) !== 0;
      if (down === buttons.has(chord)) return;
      moveTo(place);
      if (down) buttons.add(chord);
      else buttons.delete(chord);
      send({ type: "button", button: chord, down });
      return;
    }
    if (place.inside || buttons.size > 0) moveTo(place);
  };
  const onPointerDown = (event: PointerEvent) => {
    const button = POINTER_BUTTONS[event.button];
    if (!button || viewOnly) return;
    const place = locate(event);
    if (!place.inside) return;
    event.preventDefault();
    canvas.setPointerCapture(event.pointerId);
    moveTo(place);
    buttons.add(button);
    send({ type: "button", button, down: true });
  };
  const onPointerUp = (event: PointerEvent) => {
    const button = POINTER_BUTTONS[event.button];
    if (!button || !buttons.delete(button)) return;
    moveTo(locate(event));
    send({ type: "button", button, down: false });
  };
  const releaseButtons = () => {
    for (const button of buttons) send({ type: "button", button, down: false });
    buttons.clear();
  };
  const onWheel = (event: WheelEvent) => {
    if (viewOnly) return;
    event.preventDefault();
    const whole = { vertical: 0, horizontal: 0 };
    for (const axis of ["vertical", "horizontal"] as const) {
      const notches = wheelNotches(event, axis);
      if (!notches) continue;
      if (Math.sign(notches) !== Math.sign(wheelRemainder[axis])) wheelRemainder[axis] = 0;
      wheelRemainder[axis] += notches;
      whole[axis] = Math.trunc(wheelRemainder[axis]);
      wheelRemainder[axis] -= whole[axis];
    }
    if (whole.vertical || whole.horizontal)
      send({ type: "wheel", dx: whole.horizontal, dy: whole.vertical });
  };
  const onContextMenu = (event: Event) => event.preventDefault();
  canvas.addEventListener("pointermove", onPointerMove);
  canvas.addEventListener("pointerdown", onPointerDown);
  canvas.addEventListener("pointerup", onPointerUp);
  canvas.addEventListener("pointercancel", releaseButtons);
  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("contextmenu", onContextMenu);

  function shutdown() {
    if (closed) return;
    closed = true;
    clearTimeout(firstFrameTimeout);
    if (animation) cancelAnimationFrame(animation);
    document.removeEventListener("visibilitychange", onVisibility);
    canvas.removeEventListener("pointermove", onPointerMove);
    canvas.removeEventListener("pointerdown", onPointerDown);
    canvas.removeEventListener("pointerup", onPointerUp);
    canvas.removeEventListener("pointercancel", releaseButtons);
    canvas.removeEventListener("wheel", onWheel);
    canvas.removeEventListener("contextmenu", onContextMenu);
    canvas.style.touchAction = pageTouchAction;
    queue = [];
    stats.waiting = 0;
    pressed.clear();
    modifiers.clear();
    buttons.clear();
    if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)
      socket.close();
  }

  const pressKey = (entry: KeyEntry, down: boolean) => {
    if (entry.kind === "code") send({ type: "key", code: entry.code, down });
    else send({ type: "symbol", symbol: entry.symbol, down });
  };
  const releaseModifiers = () => {
    for (const code of modifiers) send({ type: "key", code, down: false });
    modifiers.clear();
  };
  const releaseKeys = () => {
    for (const entry of pressed.values())
      if (entry.kind === "symbol" || !modifiers.has(entry.code)) pressKey(entry, false);
    pressed.clear();
    releaseModifiers();
  };
  const syncModifiers = (event: KeyboardEvent) => {
    const own = keyCode(event.code);
    for (const { state, left, right } of MODIFIER_KEYS) {
      if (own === left || own === right) continue;
      const wanted = event.getModifierState(state);
      const held = modifiers.has(left) || modifiers.has(right);
      if (wanted && !held) {
        modifiers.add(left);
        send({ type: "key", code: left, down: true });
      } else if (!wanted && held) {
        for (const code of [left, right])
          if (modifiers.delete(code)) send({ type: "key", code, down: false });
      }
    }
  };
  const clipboardLetter = (event: KeyboardEvent): "c" | "x" | null => {
    if (event.altKey) return null;
    const command = apple ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
    const letter = event.key.toLowerCase();
    return command && (letter === "c" || letter === "x") ? letter : null;
  };

  const keyDown = (event: KeyboardEvent) => {
    if (event.repeat || event.isComposing || event.key === "Dead" || event.code === "CapsLock")
      return;
    const letter = clipboardLetter(event);
    if (letter && chords) {
      releaseModifiers();
      send({ type: "chord", letter, shift: event.shiftKey });
      return;
    }
    if (letter) send({ type: "copy" });
    const id = event.code || event.key;
    if (isTypedCharacter(event)) {
      if (event.altKey || event.ctrlKey) releaseModifiers();
      const entry: KeyEntry = { kind: "symbol", symbol: characterKeysym(event.key) };
      pressed.set(id, entry);
      pressKey(entry, true);
      return;
    }
    syncModifiers(event);
    const code = keyCode(event.code);
    if (code === null) return;
    const entry: KeyEntry = { kind: "code", code };
    pressed.set(id, entry);
    if (MODIFIER_CODES.has(code)) modifiers.add(code);
    pressKey(entry, true);
  };
  const keyUp = (event: KeyboardEvent) => {
    const id = event.code || event.key;
    const entry = pressed.get(id);
    if (!entry) return;
    pressed.delete(id);
    if (entry.kind === "code" && MODIFIER_CODES.has(entry.code)) {
      if (modifiers.delete(entry.code)) pressKey(entry, false);
      if (META_CODES.has(entry.code)) releaseKeys();
      return;
    }
    pressKey(entry, false);
  };

  return {
    stats,
    viewOnly: () => viewOnly,
    setViewOnly: (next) => {
      if (next) {
        releaseKeys();
        releaseButtons();
      }
      viewOnly = next;
      applyCursor();
    },
    key: (event) => {
      if (viewOnly) return;
      event.preventDefault();
      if (event.type === "keyup") keyUp(event);
      else keyDown(event);
    },
    typeText: (text) => send({ type: "text", text }),
    paste: (text, shift = false) => {
      if (viewOnly) return;
      releaseModifiers();
      if (text) send({ type: "clipboard", text });
      send({ type: "chord", letter: "v", shift });
    },
    releaseHeldInput: () => {
      releaseKeys();
      releaseButtons();
    },
    close: shutdown,
  };
}
