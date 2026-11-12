import { type Api, type ExecEvent, toInfo } from "./api.js";
import { Desktop } from "./desktop.js";
import { Files } from "./files.js";
import { toArgv } from "./shell.js";
import type {
  Exposed,
  ExposeOptions,
  RunOptions,
  RunResult,
  SandboxInfo,
  SandboxState,
  Secret,
  UpdateOptions,
} from "./types.js";

const DEFAULT_TIMEOUT_MS = 10 * 60 * 1000;
const DEFAULT_MAX_OUTPUT_BYTES = 1024 * 1024;

export type SandboxContext = {
  api: Api;
  defaultUser: string | undefined;
  baseUrl: string | null;
};

/** One microVM. Obtain it from `Kumoba.create`, `Kumoba.get` or `Kumoba.list`. */
export class Sandbox implements AsyncDisposable {
  readonly id: string;
  readonly files: Files;
  readonly desktop: Desktop;
  private current: SandboxInfo;
  private readonly defaultUser: string | undefined;

  constructor(
    private readonly context: SandboxContext,
    info: SandboxInfo
  ) {
    this.id = info.id;
    this.current = info;
    this.defaultUser = context.defaultUser;
    this.files = new Files(context.api, this.id, this.defaultUser, async (command, options) =>
      this.run(command, options)
    );
    this.desktop = new Desktop(
      async (command, options) => this.run(command, options),
      this.files,
      async (port, options) => this.expose(port, options)
    );
  }

  /** Last known state. Call `refresh()` for a fresh copy. */
  get info(): SandboxInfo {
    return this.current;
  }

  get state(): SandboxState {
    return this.current.state;
  }

  get labels(): Record<string, string> {
    return this.current.labels;
  }

  async refresh(): Promise<SandboxInfo> {
    this.current = toInfo(await this.context.api.get(this.id));
    return this.current;
  }

  /** Boot the VM. No-op when it is already running. */
  async start(): Promise<void> {
    this.current = toInfo(await this.context.api.start(this.id));
  }

  /** Shut the VM down; its disk is kept and `start()` resumes from it. */
  async stop(): Promise<void> {
    this.current = toInfo(await this.context.api.stop(this.id));
  }

  /** Destroy the VM and its disk. */
  async delete(): Promise<void> {
    await this.context.api.delete(this.id);
    this.current = { ...this.current, state: "destroyed" };
  }

  async update(options: UpdateOptions): Promise<SandboxInfo> {
    this.current = toInfo(await this.context.api.update(this.id, options));
    return this.current;
  }

  /**
   * Run a command and wait for it to finish.
   * A string runs through `/bin/sh -c`; an array is an argv.
   */
  async run(command: string | readonly string[], options: RunOptions = {}): Promise<RunResult> {
    const maxBytes = options.maxOutputBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
    const stdout = new OutputBuffer(maxBytes, options.onStdout);
    const stderr = new OutputBuffer(maxBytes, options.onStderr);
    let exit: Extract<ExecEvent, { type: "exit" }> | undefined;
    let failure: string | undefined;
    const user = options.user ?? this.defaultUser;
    await this.context.api.exec(
      this.id,
      {
        argv: toArgv(command),
        ...(options.cwd !== undefined ? { cwd: options.cwd } : {}),
        ...(options.env !== undefined ? { env: options.env } : {}),
        timeoutMs: options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
        ...(user !== undefined ? { user } : {}),
      },
      (event) => {
        switch (event.type) {
          case "stdout":
            stdout.push(event.data);
            break;
          case "stderr":
            stderr.push(event.data);
            break;
          case "exit":
            exit = event;
            break;
          case "error":
            failure = event.message;
        }
      },
      options.signal
    );
    if (failure !== undefined) throw new Error(`kumobad could not run the command: ${failure}`);
    const exitCode = exit?.exitCode ?? null;
    return {
      exitCode,
      stdout: stdout.text(),
      stderr: stderr.text(),
      timedOut: exit?.timedOut ?? false,
      ...(exit?.signal !== undefined ? { signal: exit.signal } : {}),
      ok: exitCode === 0,
      truncated: stdout.truncated || stderr.truncated,
    };
  }

  /**
   * Make a guest TCP port reachable through kumobad for a limited time.
   * `url` is set when the client was configured with a base URL; `path` always is.
   */
  async expose(port: number, options: ExposeOptions = {}): Promise<Exposed> {
    const preview = await this.context.api.preview(this.id, port, options.ttlSeconds ?? 600);
    const base = this.context.baseUrl;
    return {
      path: preview.path,
      url: base === null ? null : `${base.replace(/\/$/, "")}${preview.path}`,
      expiresAt: new Date(preview.expiresAtMs),
    };
  }

  /**
   * Attach secrets. The guest only sees placeholders in `envVars`; kumobad swaps
   * in the real value on HTTPS requests to `hosts`. Returns the mounted variable names.
   */
  async setSecrets(secrets: readonly Secret[]): Promise<string[]> {
    return this.context.api.putSecrets(this.id, secrets);
  }

  /** `await using sandbox = await kumoba.create()` deletes the sandbox on scope exit. */
  async [Symbol.asyncDispose](): Promise<void> {
    if (this.current.state === "destroyed") return;
    await this.delete();
  }
}

class OutputBuffer {
  private readonly chunks: Buffer[] = [];
  private kept = 0;
  truncated = false;
  private readonly decoder = new TextDecoder();

  constructor(
    private readonly limit: number,
    private readonly listener: ((text: string) => void) | undefined
  ) {}

  push(base64: string): void {
    const bytes = Buffer.from(base64, "base64");
    if (this.listener) this.listener(this.decoder.decode(bytes, { stream: true }));
    const room = this.limit - this.kept;
    if (room <= 0) {
      this.truncated = true;
      return;
    }
    if (bytes.length > room) {
      this.chunks.push(bytes.subarray(0, room));
      this.kept += room;
      this.truncated = true;
      return;
    }
    this.chunks.push(bytes);
    this.kept += bytes.length;
  }

  text(): string {
    return Buffer.concat(this.chunks).toString("utf8");
  }
}
