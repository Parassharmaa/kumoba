import { type Api, type ExecEvent, type ExecRequest, toInfo } from "./api.js";
import { Desktop } from "./desktop.js";
import { KumobaError } from "./errors.js";
import { Files } from "./files.js";
import { abortedError, withDeadline } from "./http.js";
import { toNetwork } from "./network.js";
import { toArgv } from "./shell.js";
import type {
  CallOptions,
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
      async (port, options) => this.expose(port, options),
      async (options) => {
        const view = await context.api.desktop(
          this.id,
          options.ttlSeconds ?? 600,
          options.viewer,
          options.signal
        );
        return { ...this.link(view), viewer: view.viewer };
      }
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

  async refresh(options: CallOptions = {}): Promise<SandboxInfo> {
    this.current = toInfo(
      await withDeadline(options, (signal) => this.context.api.get(this.id, signal))
    );
    return this.current;
  }

  /** Boot the VM. No-op when it is already running. */
  async start(options: CallOptions = {}): Promise<void> {
    this.current = toInfo(
      await withDeadline(options, (signal) => this.context.api.start(this.id, signal))
    );
  }

  /** Shut the VM down; its disk is kept and `start()` resumes from it. */
  async stop(options: CallOptions = {}): Promise<void> {
    this.current = toInfo(
      await withDeadline(options, (signal) => this.context.api.stop(this.id, signal))
    );
  }

  /** Destroy the VM and its disk. */
  async delete(options: CallOptions = {}): Promise<void> {
    await withDeadline(options, (signal) => this.context.api.delete(this.id, signal));
    this.current = { ...this.current, state: "destroyed" };
  }

  async update(options: UpdateOptions): Promise<SandboxInfo> {
    const { labels, autoStopMinutes, network } = options;
    const patch = {
      ...(labels === undefined ? {} : { labels }),
      ...(autoStopMinutes === undefined ? {} : { autoStopMinutes }),
      ...(network === undefined ? {} : { network: toNetwork(network) }),
    };
    this.current = toInfo(
      await withDeadline(options, (signal) => this.context.api.update(this.id, patch, signal))
    );
    return this.current;
  }

  /**
   * Run a command and wait for it to finish.
   * A string runs through `/bin/sh -c`; an array is an argv.
   */
  async run(command: string | readonly string[], options: RunOptions = {}): Promise<RunResult> {
    const request = execRequest(command, options, options.user ?? this.defaultUser);
    return collectRun(
      (onEvent) => this.context.api.exec(this.id, request, onEvent, options.signal),
      options
    );
  }

  /**
   * Make a guest TCP port reachable through kumobad for a limited time.
   * `url` is set when the client was configured with a base URL; `path` always is.
   */
  async expose(port: number, options: ExposeOptions = {}): Promise<Exposed> {
    const preview = await withDeadline(options, (signal) =>
      this.context.api.preview(this.id, port, options.ttlSeconds ?? 600, signal)
    );
    return this.link(preview);
  }

  private link(preview: { path: string; expiresAtMs: number }): Exposed {
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
  async setSecrets(secrets: readonly Secret[], options: CallOptions = {}): Promise<string[]> {
    return withDeadline(options, (signal) => this.context.api.putSecrets(this.id, secrets, signal));
  }

  /** `await using sandbox = await kumoba.create()` deletes the sandbox on scope exit. */
  async [Symbol.asyncDispose](): Promise<void> {
    if (this.current.state === "destroyed") return;
    await this.delete();
  }
}

export function execRequest(
  command: string | readonly string[],
  options: RunOptions,
  user: string | undefined
): ExecRequest {
  return {
    argv: toArgv(command),
    ...(options.cwd !== undefined ? { cwd: options.cwd } : {}),
    ...(options.env !== undefined ? { env: options.env } : {}),
    timeoutMs: options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
    ...(user !== undefined ? { user } : {}),
  };
}

export async function collectRun(
  stream: (onEvent: (event: ExecEvent) => void) => Promise<void>,
  options: RunOptions
): Promise<RunResult> {
  const maxBytes = options.maxOutputBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
  const stdout = new OutputBuffer(maxBytes, options.onStdout);
  const stderr = new OutputBuffer(maxBytes, options.onStderr);
  let exit: Extract<ExecEvent, { type: "exit" }> | undefined;
  let failure: string | undefined;
  await stream((event) => {
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
        break;
      case "sandbox":
        break;
    }
  });
  if (failure !== undefined) {
    throw new KumobaError(`kumobad could not run the command: ${failure}`, {
      status: 502,
      code: "daemon_error",
    });
  }
  if (exit === undefined) {
    if (options.signal?.aborted) throw abortedError(options.signal);
    throw new KumobaError("kumobad output stream ended before the command exited", {
      status: 502,
      code: "protocol",
      retryable: true,
    });
  }
  const exitCode = exit.exitCode;
  return {
    exitCode,
    stdout: stdout.text(),
    stderr: stderr.text(),
    timedOut: exit.timedOut,
    ...(exit.signal !== undefined ? { signal: exit.signal } : {}),
    ok: exitCode === 0,
    truncated: stdout.truncated || stderr.truncated,
  };
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
