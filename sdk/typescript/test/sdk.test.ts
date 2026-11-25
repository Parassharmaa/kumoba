import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Kumoba, KumobaError } from "../src/index.js";
import { toArgv } from "../src/shell.js";
import { type FakeKumobad, startFakeKumobad } from "./fake-kumobad.js";

let fake: FakeKumobad;
let kumoba: Kumoba;

beforeEach(async () => {
  fake = await startFakeKumobad();
  kumoba = new Kumoba({ url: fake.url, token: fake.token });
});

afterEach(async () => {
  await fake.close();
});

describe("Kumoba", () => {
  it("creates a started sandbox with sensible defaults", async () => {
    const sandbox = await kumoba.create();
    expect(sandbox.state).toBe("started");
    expect(sandbox.info).toMatchObject({
      image: "default",
      cpu: 2,
      memoryMib: 4096,
      autoStopMinutes: 15,
      autoDeleteMinutes: null,
      network: { mode: "open", hosts: [] },
    });
    expect(sandbox.info.createdAt).toBeInstanceOf(Date);
  });

  it("maps the network shorthand and honours start: false", async () => {
    const blocked = await kumoba.create({ network: "blocked", start: false });
    expect(blocked.state).toBe("stopped");
    expect(blocked.info.network).toEqual({ mode: "blocked", hosts: [] });
    const allow = await kumoba.create({ network: { allow: ["api.github.com"] } });
    expect(allow.info.network).toEqual({ mode: "allowlist", hosts: ["api.github.com"] });
  });

  it("lists by label, gets by id and reports missing sandboxes", async () => {
    await kumoba.create({ labels: { team: "a" } });
    const b = await kumoba.create({ labels: { team: "b" } });
    expect((await kumoba.list({ labels: { team: "b" } })).map((s) => s.id)).toEqual([b.id]);
    expect((await kumoba.get(b.id)).labels).toEqual({ team: "b" });
    expect(await kumoba.find("nope")).toBeUndefined();
    await expect(kumoba.get("nope")).rejects.toMatchObject({ notFound: true, status: 404 });
  });

  it("lists the newest sandboxes first and keeps them when the limit cuts the list", async () => {
    const [first, second, third] = [
      await kumoba.create({ labels: { batch: "order" } }),
      await kumoba.create({ labels: { batch: "order" } }),
      await kumoba.create({ labels: { batch: "order" } }),
    ];
    const list = (limit: number) =>
      kumoba.list({ labels: { batch: "order" }, limit }).then((all) => all.map((s) => s.id));
    expect(await list(10)).toEqual([third.id, second.id, first.id]);
    expect(await list(2)).toEqual([third.id, second.id]);
  });

  it("lists every matching sandbox when no limit is given", async () => {
    for (let index = 0; index < 101; index += 1)
      await kumoba.create({ labels: { job: "cleanup" } });
    await kumoba.create({ labels: { job: "other" } });
    expect(await kumoba.list({ labels: { job: "cleanup" } })).toHaveLength(101);
    expect(await kumoba.list({ labels: { job: "cleanup" }, limit: 100 })).toHaveLength(100);
  });

  it("reports whether a bounded page left sandboxes out", async () => {
    for (let index = 0; index < 3; index += 1) await kumoba.create({ labels: { job: "page" } });
    const page = (limit?: number) =>
      kumoba
        .listPage({ labels: { job: "page" }, ...(limit === undefined ? {} : { limit }) })
        .then(({ sandboxes, truncated }) => ({ count: sandboxes.length, truncated }));
    expect(await page(2)).toEqual({ count: 2, truncated: true });
    expect(await page(3)).toEqual({ count: 3, truncated: false });
    expect(await page(0)).toEqual({ count: 0, truncated: true });
    expect(await page()).toEqual({ count: 3, truncated: false });
    expect(fake.listLimits.at(-1)).toBeNull();
  });

  it("throws instead of returning part of the list when the daemon still cuts it", async () => {
    for (let index = 0; index < 3; index += 1) await kumoba.create({ labels: { job: "cap" } });
    fake.listCap = 2;
    await expect(kumoba.list({ labels: { job: "cap" } })).rejects.toMatchObject({
      code: "list_truncated",
      retryable: false,
    });
    expect(await kumoba.list({ labels: { job: "cap" }, limit: 2 })).toHaveLength(2);
    expect(await kumoba.listPage({ labels: { job: "cap" } })).toMatchObject({ truncated: true });
  });

  it.each([-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 60])(
    "refuses a limit of %s before asking the daemon",
    async (limit) => {
      await expect(kumoba.listPage({ limit })).rejects.toMatchObject({ code: "invalid_request" });
      await expect(kumoba.list({ limit })).rejects.toMatchObject({ code: "invalid_request" });
      expect(fake.listLimits).toEqual([]);
    }
  );

  it("lists by a label whose value has a colon and refuses keys the filter cannot express", async () => {
    const url = await kumoba.create({ labels: { url: "http://x:1" } });
    await kumoba.create({ labels: { url: "http://y:1" } });
    expect((await kumoba.list({ labels: { url: "http://x:1" } })).map((s) => s.id)).toEqual([
      url.id,
    ]);
    fake.listFilters = [];
    for (const key of ["", "a:b"]) {
      await expect(kumoba.list({ labels: { team: "a", [key]: "c" } })).rejects.toMatchObject({
        code: "invalid_request",
      });
    }
    expect(fake.listFilters).toEqual([]);
  });

  it("rejects a wrong token with a KumobaError", async () => {
    const wrong = new Kumoba({ url: fake.url, token: "bad" });
    await expect(wrong.images()).rejects.toBeInstanceOf(KumobaError);
    await expect(wrong.images()).rejects.toMatchObject({ status: 401, code: "unauthorized" });
  });

  it("reads configuration from the environment", () => {
    const client = Kumoba.fromEnv({ KUMOBA_URL: fake.url, KUMOBA_TOKEN: fake.token });
    expect(client).toBeInstanceOf(Kumoba);
    expect(() => Kumoba.fromEnv({ KUMOBA_HOME: "/nonexistent-kumoba-home" })).toThrow(
      /No kumobad token/
    );
  });

  it("finds a zero-config kumobad through its home directory", async () => {
    const home = mkdtempSync(join(tmpdir(), "kumoba-home-"));
    try {
      writeFileSync(join(home, "token"), `${fake.token}\n`);
      const client = new Kumoba({ url: fake.url, env: { KUMOBA_HOME: home } });
      expect(await client.images()).toEqual(await kumoba.images());
      expect(() => new Kumoba({ env: { KUMOBA_HOME: home } })).not.toThrow();
      const explicit = new Kumoba({
        url: "unix:///elsewhere/kumobad.sock",
        token: fake.token,
        env: { KUMOBA_PUBLIC_URL: "http://other.example" },
      });
      expect(explicit).toBeInstanceOf(Kumoba);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("deletes the sandbox when `await using` goes out of scope", async () => {
    let id = "";
    {
      await using sandbox = await kumoba.create();
      id = sandbox.id;
      expect(fake.sandboxes.has(id)).toBe(true);
    }
    expect(fake.sandboxes.has(id)).toBe(false);
  });
});

describe("Sandbox.run", () => {
  it("runs strings through /bin/sh and resolves bare programs through PATH", async () => {
    expect(toArgv("echo hi")).toEqual(["/bin/sh", "-c", "echo hi"]);
    expect(toArgv(["/usr/bin/python3", "-c", "1"])).toEqual(["/usr/bin/python3", "-c", "1"]);
    expect(toArgv(["python3", "-c", "1"])).toEqual([
      "/bin/sh",
      "-c",
      'exec "$0" "$@"',
      "python3",
      "-c",
      "1",
    ]);
  });

  it("returns separated stdout and stderr with the exit code", async () => {
    fake.onExec = () => ({ stdout: "out\n", stderr: "err\n", exitCode: 3 });
    const sandbox = await kumoba.create({ user: "kumoba" });
    const result = await sandbox.run("false", {
      cwd: "/workspace",
      env: { A: "1" },
      timeoutMs: 5000,
    });
    expect(result).toMatchObject({
      stdout: "out\n",
      stderr: "err\n",
      exitCode: 3,
      ok: false,
      timedOut: false,
    });
    expect(fake.execs.at(-1)).toMatchObject({
      argv: ["/bin/sh", "-c", "false"],
      cwd: "/workspace",
      env: { A: "1" },
      timeoutMs: 5000,
      user: "kumoba",
    });
  });

  it("streams output, reports timeouts and caps retained output", async () => {
    fake.onExec = () => ({ stdout: "x".repeat(100), timedOut: true });
    const sandbox = await kumoba.create();
    const seen: string[] = [];
    const result = await sandbox.run("sleep 99", {
      onStdout: (text) => seen.push(text),
      maxOutputBytes: 10,
    });
    expect(seen.join("")).toHaveLength(100);
    expect(result).toMatchObject({ exitCode: null, timedOut: true, truncated: true, ok: false });
    expect(result.stdout).toHaveLength(10);
  });

  it("surfaces daemon-side launch failures as errors", async () => {
    fake.onExec = () => ({ error: "guest agent unreachable" });
    const sandbox = await kumoba.create();
    const failure = await sandbox.run("true").catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(KumobaError);
    expect(failure).toMatchObject({ code: "daemon_error", retryable: false });
    expect((failure as Error).message).toMatch(/guest agent unreachable/);
    await expect(kumoba.run("true")).rejects.toMatchObject({ code: "daemon_error" });
  });
});

describe("run failures", () => {
  const failure = async (promise: Promise<unknown>) => {
    const error = await promise.then(
      () => undefined,
      (caught: unknown) => caught
    );
    expect(error).toBeInstanceOf(KumobaError);
    return error as KumobaError;
  };
  const execReceived = async () => {
    while (fake.execs.length === 0) await new Promise((resolve) => setTimeout(resolve, 5));
  };

  it("reports a cancelled run as aborted and not retryable, wherever it was cancelled", async () => {
    const sandbox = await kumoba.create();
    const reason = new Error("user cancelled");

    const early = new AbortController();
    early.abort(reason);
    const before = await failure(sandbox.run("true", { signal: early.signal }));
    expect(before).toMatchObject({ code: "aborted", retryable: false, cause: reason });

    fake.onExec = () => ({ cut: "before" });
    const waiting = new AbortController();
    const pending = sandbox.run("sleep 99", { signal: waiting.signal });
    await execReceived();
    waiting.abort(reason);
    expect(await failure(pending)).toMatchObject({
      code: "aborted",
      retryable: false,
      cause: reason,
    });

    fake.onExec = () => ({ stdout: "started\n", cut: "hang" });
    const streaming = new AbortController();
    const running = kumoba.run("sleep 99", {
      signal: streaming.signal,
      onStdout: () => streaming.abort(reason),
    });
    expect(await failure(running)).toMatchObject({
      code: "aborted",
      retryable: false,
      cause: reason,
    });
  });

  it("reports a connection lost mid-run as unreachable and retryable", async () => {
    fake.onExec = () => ({ stdout: "started\n", cut: "drop" });
    const sandbox = await kumoba.create();
    const seen: string[] = [];
    const lost = await failure(sandbox.run("sleep 99", { onStdout: (text) => seen.push(text) }));
    expect(lost).toMatchObject({ code: "unreachable", retryable: true });
    expect(seen).toEqual(["started\n"]);
    expect(await failure(kumoba.run("sleep 99"))).toMatchObject({ code: "unreachable" });
  });

  it("reports a reply cut off mid-body as unreachable", async () => {
    fake.imagesCut = true;
    const lost = await failure(kumoba.images());
    expect(lost).toMatchObject({ code: "unreachable", retryable: true });
    expect(lost.message).toMatch(/^Lost the connection to kumobad/);
  });

  it("rejects a run whose output ends without an exit status", async () => {
    fake.onExec = () => ({ stdout: "partial", cut: "end" });
    const sandbox = await kumoba.create();
    expect(await failure(sandbox.run("true"))).toMatchObject({ code: "protocol" });
    expect(await failure(kumoba.run("true"))).toMatchObject({ code: "protocol" });
  });

  it("passes an error thrown by an output callback through unchanged", async () => {
    fake.onExec = () => ({ stdout: "x", exitCode: 0 });
    const sandbox = await kumoba.create();
    const own = new Error("callback broke");
    await expect(
      sandbox.run("true", {
        onStdout: () => {
          throw own;
        },
      })
    ).rejects.toBe(own);
  });

  it("refuses an empty command", async () => {
    const sandbox = await kumoba.create();
    expect(await failure(sandbox.run([]))).toMatchObject({ code: "invalid_request" });
  });
});

describe("Sandbox.files", () => {
  it("writes, reads, lists and streams", async () => {
    const sandbox = await kumoba.create({ user: "kumoba" });
    await sandbox.files.write("/workspace/a.txt", "hello");
    await sandbox.files.write("/workspace/b.bin", new Uint8Array([1, 2, 3]));
    expect(await sandbox.files.readText("/workspace/a.txt")).toBe("hello");
    expect([...(await sandbox.files.read("/workspace/b.bin"))]).toEqual([1, 2, 3]);
    expect(await sandbox.files.list("/workspace")).toEqual([
      { name: "a.txt", kind: "file", size: 5 },
      { name: "b.bin", kind: "file", size: 3 },
    ]);
    const chunks: Buffer[] = [];
    for await (const chunk of await sandbox.files.stream("/workspace/a.txt"))
      chunks.push(chunk as Buffer);
    expect(Buffer.concat(chunks).toString()).toBe("hello");
    await expect(sandbox.files.read("/missing")).rejects.toMatchObject({
      status: 422,
      code: "guest_error",
    });
  });

  it.each([
    ["ends cleanly", "end"],
    ["drops the connection", "abort"],
  ] as const)("rejects a download that %s before the declared file size", async (_name, finish) => {
    const sandbox = await kumoba.create();
    await sandbox.files.write("/workspace/c.bin", Buffer.alloc(10, 7));
    fake.downloadCut = { bytes: 3, finish };
    const truncated = { code: "protocol", retryable: true };
    await expect(sandbox.files.read("/workspace/c.bin")).rejects.toMatchObject(truncated);
    const consume = async () => {
      for await (const _chunk of await sandbox.files.stream("/workspace/c.bin"));
    };
    await expect(consume()).rejects.toMatchObject(truncated);
    const directory = mkdtempSync(join(tmpdir(), "kumoba-download-"));
    try {
      await expect(
        sandbox.files.download("/workspace/c.bin", join(directory, "c.bin"))
      ).rejects.toMatchObject(truncated);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
    fake.downloadCut = undefined;
    expect((await sandbox.files.read("/workspace/c.bin")).length).toBe(10);
  });

  it("closes the connection when a caller stops a stalled stream", async () => {
    const sandbox = await kumoba.create();
    await sandbox.files.write("/workspace/c.bin", Buffer.alloc(10, 7));
    fake.downloadCut = { bytes: 3, finish: "hang" };
    const closed = async (count: number) => {
      while (fake.downloadsClosed < count) await new Promise((resolve) => setTimeout(resolve, 5));
    };
    const unread = await sandbox.files.stream("/workspace/c.bin");
    unread.destroy();
    await closed(1);
    const reading = await sandbox.files.stream("/workspace/c.bin");
    const errors: unknown[] = [];
    reading.on("error", (error) => errors.push(error));
    await new Promise((resolve) => reading.once("data", resolve));
    reading.destroy(new Error("stopped"));
    await closed(2);
    await new Promise((resolve) => setImmediate(resolve));
    expect(errors).toMatchObject([{ message: "stopped" }]);
  });

  it("implements mkdir, remove and exists through the shell as the configured user", async () => {
    fake.onExec = (call) => ({ exitCode: call.argv[2]?.startsWith("test -e") ? 1 : 0 });
    const sandbox = await kumoba.create({ user: "kumoba" });
    await sandbox.files.mkdir("/workspace/it's");
    await sandbox.files.remove("/workspace/it's");
    expect(await sandbox.files.exists("/workspace/x")).toBe(false);
    expect(fake.execs.map((call) => call.argv[2])).toEqual([
      "mkdir -p -- '/workspace/it'\\''s'",
      "rm -rf -- '/workspace/it'\\''s'",
      "test -e '/workspace/x'",
    ]);
    expect(fake.execs.every((call) => call.user === "kumoba")).toBe(true);
  });

  it("raises a KumobaError carrying the result when mkdir or remove fails", async () => {
    fake.onExec = () => ({ stderr: "mkdir: Permission denied\n", exitCode: 1 });
    const sandbox = await kumoba.create();
    for (const attempt of [sandbox.files.mkdir("/root/x"), sandbox.files.remove("/root/x")]) {
      const error = await attempt.catch((caught: unknown) => caught);
      expect(error).toBeInstanceOf(KumobaError);
      expect(error).toMatchObject({
        code: "command_failed",
        retryable: false,
        message: expect.stringMatching(/exit 1: mkdir: Permission denied/),
        result: { exitCode: 1, ok: false },
      });
    }
  });
});

describe("Sandbox extras", () => {
  it("exposes ports with a full URL only when a base URL is known", async () => {
    const sandbox = await kumoba.create();
    const link = await sandbox.expose(8080, { ttlSeconds: 60 });
    expect(link.path).toBe("/v1/preview/token-8080/");
    expect(link.url).toBeNull();
    expect(link.expiresAt).toBeInstanceOf(Date);

    const withBase = new Kumoba({
      url: fake.url,
      token: fake.token,
      publicUrl: "https://kumoba.example.com/_sandbox/",
    });
    const other = await (await withBase.get(sandbox.id)).expose(3000);
    expect(other.url).toBe("https://kumoba.example.com/_sandbox/v1/preview/token-3000/");
  });

  it("attaches secrets and deletes them globally", async () => {
    const sandbox = await kumoba.create();
    const mounted = await sandbox.setSecrets([
      {
        name: "openai",
        value: "sk-0123456789",
        hosts: ["api.openai.com"],
        envVars: ["OPENAI_API_KEY"],
      },
    ]);
    expect(mounted).toEqual(["OPENAI_API_KEY"]);
    expect(await kumoba.deleteSecrets(["openai", "ghost"])).toEqual(["openai"]);
  });

  it("drives the desktop helper and returns screenshot bytes", async () => {
    const png = Buffer.from([0x89, 0x50, 0x4e, 0x47]);
    let hasStream = true;
    fake.onExec = (call) => {
      const command = call.argv[2] ?? "";
      const args = [...command.matchAll(/'([^']*)'/g)].map((match) => match[1] ?? "");
      if (args[0] === "screenshot") {
        fake.sandboxes.get(call.sandboxId)?.files.set(args[1] ?? "", png);
        return { stdout: '{"path":"x"}\n' };
      }
      if (args[0] === "display")
        return { stdout: '{"displays":[{"isActive":true,"width":1024,"height":768}]}\n' };
      if (args[0] === "start")
        return { stdout: '{"xvfb":true,"xfce4":true,"x11vnc":true,"novnc":true}\n' };
      if (args[0] === "status" && hasStream) return { stdout: '{"running":true}\n' };
      if (args[0] === "status") return { stdout: "", exitCode: 1 };
      return { stdout: "" };
    };
    const sandbox = await kumoba.create({ user: "kumoba" });
    expect(await sandbox.desktop.start()).toMatchObject({ xvfb: true });
    expect(await sandbox.desktop.display()).toEqual({ width: 1024, height: 768 });
    expect([...(await sandbox.desktop.screenshot())]).toEqual([...png]);
    await sandbox.desktop.click(10, 20, { button: "right" });
    await sandbox.desktop.hotkey("ctrl+l");
    expect(fake.execs.map((call) => call.argv[2])).toEqual(
      expect.arrayContaining([
        "/usr/local/bin/kumoba-desktop 'click' '10' '20' 'right' '0'",
        "/usr/local/bin/kumoba-desktop 'hotkey' 'ctrl+l'",
      ])
    );
    expect(await sandbox.desktop.liveView()).toMatchObject({
      path: "/v1/preview/token-6090/",
      viewer: "stream",
    });
    expect(await sandbox.desktop.liveView({ viewer: "vnc" })).toMatchObject({
      path: "/v1/preview/token-6080/",
      viewer: "vnc",
    });
    hasStream = false;
    expect(await sandbox.desktop.liveView()).toMatchObject({
      path: "/v1/preview/token-6080/",
      viewer: "vnc",
    });
  });

  it("raises a KumobaError when the desktop helper prints something unexpected", async () => {
    const sandbox = await kumoba.create();
    for (const stdout of ["Segmentation fault\n", '{"displays":"none"}\n']) {
      fake.onExec = () => ({ stdout });
      const error = await sandbox.desktop.display().catch((caught: unknown) => caught);
      expect(error).toBeInstanceOf(KumobaError);
      expect(error).toMatchObject({ code: "protocol" });
    }
  });

  it("asks kumobad for the live view of a sandbox with no desktop helper inside", async () => {
    fake.onExec = () => ({ stdout: "", exitCode: 127 });
    const sandbox = await kumoba.create();
    const before = Date.now();
    const view = await sandbox.desktop.liveView({ ttlSeconds: 120 });
    expect(view).toMatchObject({ path: "/v1/preview/token-host/", viewer: "stream" });
    expect(view.expiresAt.getTime() - before).toBeGreaterThanOrEqual(120_000);
    expect(await sandbox.desktop.liveView({ viewer: "vnc" })).toMatchObject({
      path: "/v1/preview/token-host/",
      viewer: "stream",
    });
  });
});

describe("Sandbox.update", () => {
  it("changes the network policy of a stopped sandbox", async () => {
    const sandbox = await kumoba.create({ start: false });
    await sandbox.update({ network: { allow: ["pypi.org", "*.github.com"] }, autoStopMinutes: 3 });
    expect(sandbox.info.network).toEqual({
      mode: "allowlist",
      hosts: ["pypi.org", "*.github.com"],
    });
    expect(sandbox.info.autoStopMinutes).toBe(3);
    await sandbox.start();
    await expect(sandbox.update({ network: "blocked" })).rejects.toMatchObject({ status: 409 });
  });
});

describe("Kumoba.accessLog", () => {
  it("pages through the daemon's access log inside the window", async () => {
    const base = {
      sandboxId: "sbx-1",
      labels: { team: "a" },
      port: 443,
      decision: "allowed" as const,
      bytesIn: 10,
      bytesOut: 2,
    };
    fake.accessPageSize = 2;
    fake.accessEntries = [
      { ...base, host: "a.example", startedAtMs: 1, completedAtMs: 10 },
      { ...base, host: "b.example", startedAtMs: 2, completedAtMs: 20 },
      {
        ...base,
        host: "169.254.169.254",
        port: 80,
        decision: "denied",
        startedAtMs: 3,
        completedAtMs: 30,
      },
      {
        ...base,
        host: "silent.example",
        decision: "failed",
        bytesIn: 0,
        bytesOut: 0,
        startedAtMs: 31,
        completedAtMs: 36,
      },
      {
        ...base,
        host: "future.example",
        decision: "rerouted",
        startedAtMs: 37,
        completedAtMs: 38,
      },
      { ...base, host: "late.example", startedAtMs: 4, completedAtMs: 40 },
    ];
    const seen = [];
    for await (const entry of kumoba.accessLog({ since: new Date(5), until: new Date(40) }))
      seen.push(entry);
    expect(seen.map((entry) => [entry.host, entry.decision])).toEqual([
      ["a.example", "allowed"],
      ["b.example", "allowed"],
      ["169.254.169.254", "denied"],
      ["silent.example", "failed"],
      ["future.example", "rerouted"],
    ]);
    expect(seen[0]?.completedAt).toEqual(new Date(10));
  });
});
