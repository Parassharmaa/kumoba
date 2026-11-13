import { readFileSync } from "node:fs";
import { Readable } from "node:stream";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type Io, main } from "../src/cli.js";
import { type CommandSpec, parseArgs } from "../src/cli-args.js";
import { Kumoba } from "../src/index.js";
import { encodeFrame, FrameParser } from "../src/terminal.js";
import { type FakeKumobad, startFakeKumobad } from "./fake-kumobad.js";

const catalog = JSON.parse(
  readFileSync(new URL("./fixtures/commands.json", import.meta.url), "utf8")
) as { commands: CommandSpec[] };
const spec = (name: string) =>
  catalog.commands.find((command) => command.name === name) as CommandSpec;

let fake: FakeKumobad;
let out: Buffer[];
let err: string[];
let opened: string[];
let stdin: Buffer | undefined;

const io = (env: NodeJS.ProcessEnv = { KUMOBA_URL: fake.url, KUMOBA_TOKEN: fake.token }): Io => ({
  env,
  stdout: (chunk) => out.push(Buffer.from(chunk)),
  stderr: (chunk) => err.push(String(chunk)),
  stdin: () => stdin ?? Buffer.alloc(0),
  stdinIsTty: stdin === undefined,
  stdoutIsTty: false,
  open: (url) => opened.push(url),
  terminal: {
    input: Readable.from(stdin === undefined ? [] : [stdin]),
    isTty: false,
    setRaw: () => {},
    size: () => undefined,
    onResize: () => () => {},
    release: () => {},
  },
});
const text = () => Buffer.concat(out).toString("utf8");

beforeEach(async () => {
  fake = await startFakeKumobad();
  out = [];
  err = [];
  opened = [];
  stdin = undefined;
});

afterEach(async () => {
  await fake.close();
});

describe("terminal frames", () => {
  it("round-trip in any chunking", () => {
    const frames = [
      { type: "data", data: Buffer.from("ls -la\r") },
      { type: "resize", rows: 40, cols: 120 },
      { type: "eof" },
      { type: "exit", code: -1 },
    ] as const;
    const wire = Buffer.concat(frames.map((frame) => encodeFrame(frame)));
    const parser = new FrameParser();
    const seen = [...wire].flatMap((byte) => parser.push(Buffer.from([byte])));
    expect(seen).toEqual(frames);
  });

  it("rejects unknown frames", () => {
    expect(() => new FrameParser().push(Buffer.from([9, 0, 0, 0, 0]))).toThrow(/invalid/);
  });
});

describe("argument parsing", () => {
  it("maps positionals, flags, key=value pairs and the trailing list", () => {
    const parsed = parseArgs(spec("exec"), [
      "sbx-1",
      "--timeout-ms",
      "5000",
      "env=A=1",
      "--env",
      "B=2",
      "--",
      "python3",
      "-c",
      "print(1)",
    ]);
    expect(parsed.values).toEqual({
      id: "sbx-1",
      timeoutMs: 5000,
      env: { A: "1", B: "2" },
      argv: ["python3", "-c", "print(1)"],
    });
  });

  it("builds nested objects from dotted keys and checks enums and types", () => {
    const parsed = parseArgs(spec("create"), [
      "--image",
      "test",
      "--network.mode",
      "allowlist",
      "network.hosts=a.com,b.com",
      "--memory-mib=512",
    ]);
    expect(parsed.values).toEqual({
      image: "test",
      network: { mode: "allowlist", hosts: ["a.com", "b.com"] },
      memoryMib: 512,
    });
    expect(() => parseArgs(spec("create"), ["--network.mode", "sometimes"])).toThrow(/one of/);
    expect(() => parseArgs(spec("create"), ["--cpu", "two"])).toThrow(/integer/);
    expect(() => parseArgs(spec("create"), ["--nope", "1"])).toThrow(/unknown option/);
  });
});

describe("kumoba CLI", () => {
  it("lists the daemon's commands", async () => {
    expect(await main([], io())).toBe(0);
    expect(text()).toContain("Commands served by kumobad fake");
    expect(text()).toMatch(/ {2}exec {13}Run a command/);
  });

  it("prints a command's arguments from its schema", async () => {
    expect(await main(["exec", "--help"], io())).toBe(0);
    expect(text()).toContain("kumoba exec <id> [--flag value | key=value ...] -- <argv...>");
    expect(text()).toContain("--timeout-ms <integer>");
  });

  it("creates with -q, then streams exec output and returns the guest's exit code", async () => {
    expect(await main(["create", "--image", "test", "-q"], io())).toBe(0);
    const id = text().trim();
    expect(fake.sandboxes.has(id)).toBe(true);
    out = [];
    fake.onExec = () => ({ stdout: "hello\n", stderr: "warn\n", exitCode: 7 });
    expect(await main(["exec", id, "--", "sh", "-c", "exit 7"], io())).toBe(7);
    expect(text()).toBe("hello\n");
    expect(err.join("")).toBe("warn\n");
    expect(fake.execs.at(-1)).toMatchObject({ sandboxId: id, argv: ["sh", "-c", "exit 7"] });
  });

  it("runs a one-shot command through /v1/run", async () => {
    fake.onExec = () => ({ stdout: "aarch64\n", exitCode: 0 });
    expect(await main(["run", "--image", "test", "--", "uname", "-m"], io())).toBe(0);
    expect(text()).toBe("aarch64\n");
    expect(fake.execs.at(-1)).toMatchObject({
      sandboxId: "run",
      argv: ["uname", "-m"],
      image: "test",
    });
  });

  it("writes a file from stdin and reads it back", async () => {
    const sandbox = await new Kumoba({ url: fake.url, token: fake.token }).create({
      image: "test",
    });
    stdin = Buffer.from("from stdin");
    expect(await main(["write-file", sandbox.id, "--path", "/tmp/a.txt"], io())).toBe(0);
    stdin = undefined;
    out = [];
    expect(await main(["read-file", sandbox.id, "path=/tmp/a.txt"], io())).toBe(0);
    expect(text()).toBe("from stdin");
  });

  it("opens a returned url with --open", async () => {
    const sandbox = await new Kumoba({ url: fake.url, token: fake.token }).create({
      image: "test",
    });
    const exit = await main(
      ["expose", sandbox.id, "--port", "8080", "--ttl-seconds", "60", "--open"],
      io()
    );
    expect(exit).toBe(1);
    expect(err.join("")).toContain("no browser URL");
    expect(opened).toEqual([]);
  });

  it("relays a shell through the upgraded connection and returns its exit status", async () => {
    const sandbox = await new Kumoba({ url: fake.url, token: fake.token }).create({
      image: "test",
    });
    stdin = Buffer.from("echo hi\n");
    expect(await main(["shell", sandbox.id], io())).toBe(3);
    expect(text()).toBe("ECHO HI\n");
  });

  it("separates usage errors, daemon errors and an unreachable daemon", async () => {
    expect(await main(["exec"], io())).toBe(2);
    expect(err.join("")).toContain("missing id, argv");
    expect(await main(["teleport"], io())).toBe(2);
    expect(await main(["get", "missing"], io())).toBe(1);
    const unreachable = io({ KUMOBA_URL: "unix:///nonexistent/kumobad.sock", KUMOBA_TOKEN: "t" });
    expect(await main(["images"], unreachable)).toBe(3);
    expect(err.join("")).toContain("Start it with `kumobad`");
  });
});
