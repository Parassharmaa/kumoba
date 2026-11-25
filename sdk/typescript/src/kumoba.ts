import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { Api, toInfo } from "./api.js";
import { KumobaError } from "./errors.js";
import { CLEANUP_TIMEOUT_MS, deadline, withDeadline } from "./http.js";
import { toNetwork } from "./network.js";
import { collectRun, execRequest, Sandbox, type SandboxContext } from "./sandbox.js";
import type {
  AccessEntry,
  AccessLogOptions,
  CallOptions,
  CreateOptions,
  ListOptions,
  RunOptions,
  RunResult,
} from "./types.js";

export type OneShotOptions = RunOptions &
  Pick<CreateOptions, "image" | "cpu" | "memoryMib" | "labels" | "network">;

export type ListPage = { sandboxes: Sandbox[]; truncated: boolean };

export type KumobaOptions = {
  /**
   * `unix:///path/to/kumobad.sock` or `http://127.0.0.1:<port>`.
   * Defaults to `KUMOBA_URL`, then the socket of a zero-config kumobad in `~/.kumoba`.
   */
  url?: string;
  /**
   * Bearer token, the contents of the daemon's token file.
   * Defaults to `KUMOBA_TOKEN`, `KUMOBA_TOKEN_FILE`, then `~/.kumoba/token`.
   */
  token?: string;
  /**
   * Base URL browsers reach kumobad's preview listener at, used for `expose()` and `liveView()` links
   * instead of the link kumobad returns. Defaults to `KUMOBA_PUBLIC_URL` when `url` is not passed either.
   */
  publicUrl?: string;
  /** Guest user that commands and file operations act as. Per-sandbox `user` overrides it. Defaults to `KUMOBA_USER`. */
  defaultUser?: string;
  /** Environment to read the defaults from. Defaults to `process.env`. */
  env?: NodeJS.ProcessEnv;
};

export type GetOptions = CallOptions & {
  /** Guest user that commands and file operations act as. */
  user?: string;
};

type ResolvedOptions = { url: string; token: string; publicUrl?: string; defaultUser?: string };

function kumobaHome(env: NodeJS.ProcessEnv): string {
  return env.KUMOBA_HOME ?? join(homedir(), ".kumoba");
}

export function resolveOptions(options: KumobaOptions): ResolvedOptions {
  const env = options.env ?? process.env;
  const home = kumobaHome(env);
  const url = options.url ?? env.KUMOBA_URL ?? `unix://${join(home, "kumobad.sock")}`;
  const tokenFile = env.KUMOBA_TOKEN_FILE ?? join(home, "token");
  let token = options.token ?? env.KUMOBA_TOKEN;
  if (!token && existsSync(tokenFile)) token = readFileSync(tokenFile, "utf8").trim();
  if (!token)
    throw new KumobaError(
      `No kumobad token: pass { token }, set KUMOBA_TOKEN or KUMOBA_TOKEN_FILE, or start kumobad once so it writes ${tokenFile}`,
      { code: "config_invalid" }
    );
  const publicUrl =
    options.publicUrl ?? (options.url === undefined ? env.KUMOBA_PUBLIC_URL : undefined);
  const defaultUser = options.defaultUser ?? env.KUMOBA_USER;
  return {
    url,
    token,
    ...(publicUrl ? { publicUrl } : {}),
    ...(defaultUser ? { defaultUser } : {}),
  };
}

/** Entry point. Talks to one kumobad daemon. */
export class Kumoba {
  private readonly api: Api;
  private readonly defaultUser: string | undefined;
  private readonly publicUrl: string | null;

  /**
   * With no options, connects to the kumobad that `KUMOBA_URL` names or, failing that,
   * to a zero-config kumobad in `~/.kumoba`.
   */
  constructor(options: KumobaOptions = {}) {
    const resolved = resolveOptions(options);
    this.api = new Api(resolved.url, resolved.token);
    this.defaultUser = resolved.defaultUser;
    this.publicUrl = resolved.publicUrl ?? null;
  }

  /** Same as `new Kumoba({ env })`: configure from `KUMOBA_*` variables with `~/.kumoba` defaults. */
  static fromEnv(env: NodeJS.ProcessEnv = process.env): Kumoba {
    return new Kumoba({ env });
  }

  private context(user: string | undefined): SandboxContext {
    return { api: this.api, defaultUser: user ?? this.defaultUser, publicUrl: this.publicUrl };
  }

  /**
   * Run one command in a fresh sandbox that kumobad deletes as soon as the command ends.
   * A string runs through `/bin/sh -c`; an array is the program and its arguments.
   */
  async run(command: string | readonly string[], options: OneShotOptions = {}): Promise<RunResult> {
    const request = {
      ...execRequest(command, options, options.user ?? this.defaultUser),
      ...(options.image !== undefined ? { image: options.image } : {}),
      ...(options.cpu !== undefined ? { cpu: options.cpu } : {}),
      ...(options.memoryMib !== undefined ? { memoryMib: options.memoryMib } : {}),
      ...(options.labels !== undefined ? { labels: options.labels } : {}),
      ...(options.network !== undefined ? { network: toNetwork(options.network) } : {}),
    };
    return collectRun((onEvent) => this.api.run(request, onEvent, options.signal), options);
  }

  /** Names of the images kumobad can boot. */
  async images(options: CallOptions = {}): Promise<string[]> {
    return withDeadline(options, (signal) => this.api.images(signal));
  }

  /** Create a sandbox and, unless `start: false`, boot it. */
  async create(options: CreateOptions = {}): Promise<Sandbox> {
    return withDeadline(options, async (signal) => {
      const summary = await this.api.create(
        {
          image: options.image ?? "default",
          labels: options.labels ?? {},
          cpu: options.cpu ?? 2,
          memoryMib: options.memoryMib ?? 4096,
          autoStopMinutes: options.autoStopMinutes ?? 15,
          autoDeleteMinutes:
            options.autoDeleteMinutes === undefined ? null : options.autoDeleteMinutes,
          network: toNetwork(options.network),
        },
        signal
      );
      const sandbox = new Sandbox(this.context(options.user), toInfo(summary));
      if (options.start ?? true) {
        try {
          await sandbox.start({ signal });
        } catch (error) {
          await sandbox.delete({ timeoutMs: CLEANUP_TIMEOUT_MS }).catch(() => undefined);
          throw error;
        }
      }
      return sandbox;
    });
  }

  /** Reconnect to an existing sandbox by id. Throws a `KumobaError` with `notFound` when it is gone. */
  async get(id: string, options: GetOptions = {}): Promise<Sandbox> {
    const summary = await withDeadline(options, (signal) => this.api.get(id, signal));
    return new Sandbox(this.context(options.user), toInfo(summary));
  }

  /** Like `get`, but resolves to `undefined` instead of throwing when the sandbox does not exist. */
  async find(id: string, options: GetOptions = {}): Promise<Sandbox | undefined> {
    try {
      return await this.get(id, options);
    } catch (error) {
      if (error instanceof KumobaError && error.notFound) return undefined;
      throw error;
    }
  }

  /**
   * Sandboxes whose labels include every entry of `labels`, newest first. Without
   * `limit` it returns every match, or throws a `KumobaError` with code
   * `list_truncated` if the daemon still cut the list. With `limit` it returns at
   * most that many and does not say whether more matched; use `listPage` for that.
   */
  async list(options: ListOptions = {}): Promise<Sandbox[]> {
    if (options.limit !== undefined) return (await this.listPage(options)).sandboxes;
    const page = await this.listPage({ ...options, limit: Number.MAX_SAFE_INTEGER });
    if (page.truncated) {
      throw new KumobaError(
        `kumobad returned ${String(page.sandboxes.length)} sandboxes and said more matched; use listPage() to read a bounded page`,
        { code: "list_truncated" }
      );
    }
    return page.sandboxes;
  }

  /**
   * At most `limit` matching sandboxes, newest first (the daemon's default of 1000
   * when `limit` is left out), and whether more matched.
   */
  async listPage(options: ListOptions = {}): Promise<ListPage> {
    const { sandboxes, truncated } = await withDeadline(options, (signal) =>
      this.api.list(options.labels ?? {}, options.limit, signal)
    );
    return {
      sandboxes: sandboxes.map((summary) => new Sandbox(this.context(undefined), toInfo(summary))),
      truncated,
    };
  }

  /**
   * Every proxied connection attempt across all sandboxes in a time window, oldest
   * first. kumobad keeps 72 hours; pages past `limit` are fetched automatically.
   */
  async *accessLog(options: AccessLogOptions = {}): AsyncGenerator<AccessEntry, void, undefined> {
    const untilMs = (options.until ?? new Date()).getTime();
    const sinceMs = (options.since ?? new Date(untilMs - 2 * 3_600_000)).getTime();
    const limit = options.limit ?? 10_000;
    const { signal, clear } = deadline(options);
    try {
      let cursor: string | undefined;
      for (;;) {
        const page = await this.api.accessLog(
          { sinceMs, untilMs, limit, ...(cursor ? { cursor } : {}) },
          signal
        );
        for (const entry of page.entries) {
          const { startedAtMs, completedAtMs, dropped, ...rest } = entry;
          yield {
            ...rest,
            startedAt: new Date(startedAtMs),
            completedAt: new Date(completedAtMs),
            ...(dropped === undefined ? {} : { dropped }),
          };
        }
        if (!page.truncated || !page.nextCursor) return;
        cursor = page.nextCursor;
      }
    } finally {
      clear();
    }
  }

  /** Forget secrets by name across every sandbox. Returns the names that existed. */
  async deleteSecrets(names: readonly string[], options: CallOptions = {}): Promise<string[]> {
    return withDeadline(options, (signal) => this.api.deleteSecrets(names, signal));
  }
}
