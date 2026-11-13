export const SHELL_PROTOCOL = "kumoba-shell.v1";

const DATA = 1;
const RESIZE = 2;
const EOF = 3;
const EXIT = 4;
const MAX_FRAME = 1024 * 1024;

export type Frame =
  | { type: "data"; data: Buffer }
  | { type: "resize"; rows: number; cols: number }
  | { type: "eof" }
  | { type: "exit"; code: number };

export function encodeFrame(frame: Frame): Buffer {
  let kind: number;
  let payload: Buffer;
  switch (frame.type) {
    case "data":
      kind = DATA;
      payload = frame.data;
      break;
    case "resize":
      kind = RESIZE;
      payload = Buffer.alloc(4);
      payload.writeUInt16BE(frame.rows, 0);
      payload.writeUInt16BE(frame.cols, 2);
      break;
    case "eof":
      kind = EOF;
      payload = Buffer.alloc(0);
      break;
    case "exit":
      kind = EXIT;
      payload = Buffer.alloc(4);
      payload.writeInt32BE(frame.code, 0);
      break;
  }
  const header = Buffer.alloc(5);
  header.writeUInt8(kind, 0);
  header.writeUInt32BE(payload.length, 1);
  return Buffer.concat([header, payload]);
}

export class FrameParser {
  private pending = Buffer.alloc(0);

  push(chunk: Buffer): Frame[] {
    this.pending = Buffer.concat([this.pending, chunk]);
    const frames: Frame[] = [];
    while (this.pending.length >= 5) {
      const kind = this.pending.readUInt8(0);
      const length = this.pending.readUInt32BE(1);
      if (length > MAX_FRAME) throw new Error("terminal frame too large");
      if (this.pending.length < 5 + length) break;
      const payload = this.pending.subarray(5, 5 + length);
      this.pending = this.pending.subarray(5 + length);
      if (kind === DATA) frames.push({ type: "data", data: Buffer.from(payload) });
      else if (kind === RESIZE && length === 4) {
        frames.push({
          type: "resize",
          rows: payload.readUInt16BE(0),
          cols: payload.readUInt16BE(2),
        });
      } else if (kind === EOF) frames.push({ type: "eof" });
      else if (kind === EXIT && length === 4)
        frames.push({ type: "exit", code: payload.readInt32BE(0) });
      else throw new Error("invalid terminal frame");
    }
    return frames;
  }
}
