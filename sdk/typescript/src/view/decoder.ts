import { CODEC } from "./codec.js";

export type Decoder = {
  memory: WebAssembly.Memory;
  kv_open: (width: number, height: number) => number;
  kv_input: (length: number) => number;
  kv_decode: () => number;
  kv_dirty: (index: number) => number;
  kv_pixels: () => number;
};

const EXPORTS = ["kv_open", "kv_input", "kv_decode", "kv_dirty", "kv_pixels"] as const;

function isDecoder(exports: WebAssembly.Exports): exports is WebAssembly.Exports & Decoder {
  return (
    exports.memory instanceof WebAssembly.Memory &&
    EXPORTS.every((name) => typeof exports[name] === "function")
  );
}

function codecBytes(): Uint8Array<ArrayBuffer> {
  const text = atob(CODEC);
  const bytes = new Uint8Array(text.length);
  for (let index = 0; index < text.length; index++) bytes[index] = text.charCodeAt(index);
  return bytes;
}

let compiled: Promise<WebAssembly.Module> | null = null;

export async function loadDecoder(): Promise<WebAssembly.Module> {
  compiled ??= WebAssembly.compile(codecBytes());
  try {
    return await compiled;
  } catch (error: unknown) {
    compiled = null;
    throw error;
  }
}

export async function createDecoder(): Promise<Decoder> {
  const instance = await WebAssembly.instantiate(await loadDecoder(), {});
  if (!isDecoder(instance.exports)) throw new Error("The live view decoder is not usable");
  return instance.exports;
}
