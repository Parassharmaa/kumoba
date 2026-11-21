import { writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { Kumoba } from "../src/index.js";

const live = process.env.KUMOBA_DESKTOP_LIVE === "1";

type Wasm = { instantiate(bytes: ArrayBuffer): Promise<{ instance: { exports: unknown } }> };

type Codec = {
  memory: { buffer: ArrayBuffer };
  kv_open(width: number, height: number): number;
  kv_input(length: number): number;
  kv_decode(): number;
  kv_pixels(): number;
};

async function firstFrame(base: string) {
  const wasm = (globalThis as unknown as { WebAssembly: Wasm }).WebAssembly;
  const module = await wasm.instantiate(
    await (await fetch(new URL("codec.wasm", base))).arrayBuffer()
  );
  const codec = module.instance.exports as Codec;
  const url = new URL("stream", base);
  url.protocol = "ws:";
  const socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  try {
    return await new Promise<{ width: number; height: number; colours: number }>(
      (resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("the stream sent no frame")), 30_000);
        let size = { width: 0, height: 0 };
        socket.onerror = () => reject(new Error("the stream did not open"));
        socket.onmessage = (event) => {
          const view = new DataView(event.data as ArrayBuffer);
          if (view.getUint8(0) === 1) {
            size = { width: view.getUint16(1, true), height: view.getUint16(3, true) };
            if (!codec.kv_open(size.width, size.height)) reject(new Error("unsupported size"));
            return;
          }
          if (view.getUint8(0) !== 2) return;
          clearTimeout(timer);
          const data = new Uint8Array(event.data as ArrayBuffer, 13);
          new Uint8Array(codec.memory.buffer, codec.kv_input(data.byteLength)).set(data);
          if (!codec.kv_decode()) return reject(new Error("the first frame did not decode"));
          const pixels = new Uint32Array(
            codec.memory.buffer,
            codec.kv_pixels(),
            size.width * size.height
          );
          resolve({ ...size, colours: new Set(pixels).size });
        };
      }
    );
  } finally {
    socket.close();
  }
}

describe.skipIf(!live)("default image desktop", () => {
  it("boots the default image, starts the desktop and takes a screenshot", async () => {
    const kumoba = Kumoba.fromEnv();
    await using sandbox = await kumoba.create({
      image: "default",
      cpu: 2,
      memoryMib: 2048,
      user: "kumoba",
    });

    const tools = await sandbox.run(
      "python3 --version && node --version && chromium --version && id -un"
    );
    expect(tools.ok, tools.stderr).toBe(true);
    expect(tools.stdout).toMatch(/Python 3/);
    expect(tools.stdout).toMatch(/\nv\d+\./);
    expect(tools.stdout).toMatch(/Chromium/);
    expect(tools.stdout.trim().endsWith("kumoba")).toBe(true);

    const started = await sandbox.desktop.start();
    expect(started).toMatchObject({
      xvfb: true,
      xfce4: true,
      x11vnc: true,
      novnc: true,
      view: true,
    });
    expect(await sandbox.desktop.display()).toEqual({ width: 1024, height: 768 });

    await sandbox.run("setsid xfce4-terminal >/dev/null 2>&1 &", { env: { DISPLAY: ":0" } });
    await sandbox.run("sleep 2");
    await sandbox.desktop.type("echo kumoba-desktop-ok\n");
    await sandbox.run("sleep 1");

    const png = await sandbox.desktop.screenshot({ cursor: true });
    expect([...png.subarray(0, 4)]).toEqual([0x89, 0x50, 0x4e, 0x47]);
    if (process.env.KUMOBA_SCREENSHOT) writeFileSync(process.env.KUMOBA_SCREENSHOT, png);

    const jpeg = await sandbox.desktop.screenshot({ format: "jpeg", scale: 0.5, quality: 60 });
    expect(jpeg.length).toBeGreaterThan(0);
    expect(jpeg.length).toBeLessThan(png.length);

    await sandbox.desktop.click(10, 10);
    expect(await sandbox.desktop.position()).toEqual({ x: 10, y: 10 });

    const tree = await sandbox.desktop.accessibilityTree({ maxDepth: 3 });
    expect(tree.root).not.toBeNull();

    const vnc = await sandbox.desktop.liveView({ ttlSeconds: 60, viewer: "vnc" });
    expect(vnc.path).toMatch(/^\/v1\/preview\//);
    expect(vnc.viewer).toBe("vnc");

    const view = await sandbox.desktop.liveView({ ttlSeconds: 60 });
    expect(view.viewer).toBe("stream");
    expect(view.url, "the daemon needs a preview listener").not.toBeNull();
    const base = view.url ?? "";
    expect(await (await fetch(base)).text()).toContain("viewer.js");
    const frame = await firstFrame(base);
    expect(frame).toMatchObject({ width: 1024, height: 768 });
    expect(frame.colours).toBeGreaterThan(8);
  }, 600_000);
});
