import { describe, expect, it } from "vitest";
import { Kumoba } from "../src/index.js";

const live = process.env.KUMOBA_LIVE === "1";
const image = process.env.KUMOBA_TEST_IMAGE_NAME ?? "test";

describe.skipIf(!live)("against a real kumobad", () => {
  it("boots, runs commands, moves files, exposes a port and cleans up", async () => {
    const kumoba = Kumoba.fromEnv();
    expect(await kumoba.images()).toContain(image);

    await using sandbox = await kumoba.create({
      image,
      cpu: 1,
      memoryMib: 512,
      network: "blocked",
    });
    expect(sandbox.state).toBe("started");

    const hello = await sandbox.run("echo hello; echo oops >&2; exit 3");
    expect(hello).toMatchObject({ stdout: "hello\n", stderr: "oops\n", exitCode: 3, ok: false });

    const argv = await sandbox.run(["uname", "-s"]);
    expect(argv.stdout.trim()).toBe("Linux");

    const asUser = await sandbox.run("id -un; pwd", { user: "kumoba", cwd: "/home/kumoba" });
    expect(asUser.stdout).toBe("kumoba\n/home/kumoba\n");

    const seen: string[] = [];
    const streamed = await sandbox.run("for i in 1 2 3; do echo $i; done", {
      onStdout: (text) => seen.push(text),
    });
    expect(seen.join("")).toBe("1\n2\n3\n");
    expect(streamed.ok).toBe(true);

    const slow = await sandbox.run("sleep 30", { timeoutMs: 500 });
    expect(slow.timedOut).toBe(true);

    const bytes = Buffer.alloc(2_500_000, 7);
    await sandbox.files.mkdir("/workspace/up");
    await sandbox.files.write("/workspace/up/blob", bytes);
    expect((await sandbox.files.read("/workspace/up/blob")).equals(bytes)).toBe(true);
    expect(await sandbox.files.list("/workspace/up")).toEqual([
      { name: "blob", kind: "file", size: bytes.length },
    ]);
    expect(await sandbox.files.exists("/workspace/up/blob")).toBe(true);
    await sandbox.files.remove("/workspace/up");
    expect(await sandbox.files.exists("/workspace/up")).toBe(false);

    await sandbox.run(
      "echo preview-ok > /workspace/index.html && setsid /usr/local/bin/http-fixture >/dev/null 2>&1 & while [ ! -f /workspace/http-ready ]; do sleep 0.1; done"
    );
    const link = await sandbox.expose(8080, { ttlSeconds: 60 });
    expect(link.path).toMatch(/^\/v1\/preview\/.+\/$/);
    if (link.url) {
      const response = await fetch(`${link.url}index.html`);
      expect(await response.text()).toContain("preview-ok");
    }

    await sandbox.stop();
    expect(sandbox.state).toBe("stopped");
    await sandbox.start();
    expect((await sandbox.run("echo back")).stdout).toBe("back\n");
    const id = sandbox.id;
    await sandbox.delete();
    expect(await kumoba.find(id)).toBeUndefined();
  }, 120_000);
});
