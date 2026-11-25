import { createReadStream, createWriteStream } from "node:fs";
import type { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import type { Api } from "./api.js";
import { KumobaError } from "./errors.js";
import { readBody, withDeadline } from "./http.js";
import { shellQuote } from "./shell.js";
import type { FileEntry, FileOptions, ReadOptions, RunOptions, RunResult } from "./types.js";

export type FileContent = string | Uint8Array | Readable | AsyncIterable<Uint8Array>;

const MAX_READ_BYTES = 64 * 1024 * 1024;

function toBody(content: FileContent): AsyncIterable<Uint8Array> | Iterable<Uint8Array> {
  if (typeof content === "string") return [Buffer.from(content, "utf8")];
  if (content instanceof Uint8Array) return [content];
  return content as AsyncIterable<Uint8Array>;
}

async function* readLocal(path: string): AsyncGenerator<Uint8Array> {
  for await (const chunk of createReadStream(path)) yield chunk as Buffer;
}

/** File operations on one sandbox. Reached as `sandbox.files`. */
export class Files {
  constructor(
    private readonly api: Api,
    private readonly id: string,
    private readonly defaultUser: string | undefined,
    private readonly sh: (
      command: string,
      options: Pick<RunOptions, "user" | "signal">
    ) => Promise<RunResult>
  ) {}

  private user(options?: FileOptions): string | undefined {
    return options?.user ?? this.defaultUser;
  }

  /** Write a string, bytes or a stream to `path`. Parent directories must exist. */
  async write(path: string, content: FileContent, options?: FileOptions): Promise<void> {
    await withDeadline(options, (signal) =>
      this.api.upload(this.id, path, toBody(content), this.user(options), signal)
    );
  }

  /** Read a whole file into memory. */
  async read(path: string, options?: ReadOptions): Promise<Buffer> {
    return withDeadline(options, async (signal) => {
      const response = await this.api.download(this.id, path, this.user(options), signal);
      return readBody(response, options?.maxBytes ?? MAX_READ_BYTES);
    });
  }

  /** Read a whole file as UTF-8 text. */
  async readText(path: string, options?: ReadOptions): Promise<string> {
    return (await this.read(path, options)).toString("utf8");
  }

  /**
   * Stream a file without buffering it. `timeoutMs` bounds opening the stream;
   * `signal` can stop it at any point.
   */
  async stream(path: string, options?: FileOptions): Promise<Readable> {
    return withDeadline(options, (signal) =>
      this.api.download(this.id, path, this.user(options), signal)
    );
  }

  async list(path: string, options?: FileOptions): Promise<FileEntry[]> {
    return withDeadline(options, (signal) =>
      this.api.listFiles(this.id, path, this.user(options), signal)
    );
  }

  /** Copy a local file into the sandbox. */
  async upload(localPath: string, remotePath: string, options?: FileOptions): Promise<void> {
    await this.write(remotePath, readLocal(localPath), options);
  }

  /** Copy a sandbox file to the local filesystem. */
  async download(remotePath: string, localPath: string, options?: FileOptions): Promise<void> {
    await withDeadline(options, async (signal) => {
      const user = this.user(options);
      await pipeline(
        await this.api.download(this.id, remotePath, user, signal),
        createWriteStream(localPath)
      );
    });
  }

  /** `mkdir -p` inside the guest. */
  async mkdir(path: string, options?: FileOptions): Promise<void> {
    await this.must(`mkdir -p -- ${shellQuote(path)}`, options);
  }

  /** `rm -rf` inside the guest. */
  async remove(path: string, options?: FileOptions): Promise<void> {
    await this.must(`rm -rf -- ${shellQuote(path)}`, options);
  }

  async exists(path: string, options?: FileOptions): Promise<boolean> {
    return (await this.shell(`test -e ${shellQuote(path)}`, options)).ok;
  }

  private async must(command: string, options?: FileOptions): Promise<void> {
    const result = await this.shell(command, options);
    if (!result.ok) throw commandError(command, result);
  }

  private async shell(command: string, options?: FileOptions): Promise<RunResult> {
    return withDeadline(options, (signal) =>
      this.sh(command, {
        ...(options?.user === undefined ? {} : { user: options.user }),
        ...(signal === undefined ? {} : { signal }),
      })
    );
  }
}

function commandError(command: string, result: RunResult): KumobaError & { result: RunResult } {
  const detail = result.stderr.trim() || result.stdout.trim();
  const message = `${command.split(" ")[0] ?? command} failed with exit ${String(result.exitCode)}${detail ? `: ${detail}` : ""}`;
  return Object.assign(new KumobaError(message, { status: 502, code: "command_failed" }), {
    result,
  });
}
