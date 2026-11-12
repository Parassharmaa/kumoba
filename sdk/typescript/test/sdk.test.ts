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

  it("rejects a wrong token with a KumobaError", async () => {
    const wrong = new Kumoba({ url: fake.url, token: "bad" });
    await expect(wrong.images()).rejects.toBeInstanceOf(KumobaError);
    await expect(wrong.images()).rejects.toMatchObject({ status: 401, code: "unauthorized" });
  });

  it("reads configuration from the environment", () => {
    const client = Kumoba.fromEnv({ KUMOBA_URL: fake.url, KUMOBA_TOKEN: fake.token });
    expect(client).toBeInstanceOf(Kumoba);
    expect(() => Kumoba.fromEnv({})).toThrow(/KUMOBA_URL/);
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
    await expect(sandbox.run("true")).rejects.toThrow(/guest agent unreachable/);
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
    expect((await sandbox.desktop.liveView()).path).toBe("/v1/preview/token-6080/");
  });
});
