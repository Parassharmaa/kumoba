import { writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { Kumoba } from "../src/index.js";

const live = process.env.KUMOBA_DESKTOP_LIVE === "1";

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
    expect(started).toMatchObject({ xvfb: true, xfce4: true, x11vnc: true, novnc: true });
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

    const view = await sandbox.desktop.liveView({ ttlSeconds: 60 });
    expect(view.path).toMatch(/^\/v1\/preview\//);
  }, 600_000);
});
