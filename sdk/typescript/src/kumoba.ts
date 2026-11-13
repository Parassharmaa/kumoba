import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { Api, toInfo } from "./api.js";
import { KumobaError } from "./errors.js";
import { toNetwork } from "./network.js";
import { Sandbox, type SandboxContext } from "./sandbox.js";
import type { AccessEntry, AccessLogOptions, CreateOptions, ListOptions } from "./types.js";

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
   * Base URL browsers can reach kumobad through, used to build `expose()` links.
   * Defaults to `KUMOBA_PUBLIC_URL`, then the `http://` URL when `url` is one, otherwise links carry only a path.
   */
  publicUrl?: string;
  /** Guest user that commands and file operations act as. Per-sandbox `user` overrides it. Defaults to `KUMOBA_USER`. */
  defaultUser?: string;
  /** Environment to read the defaults from. Defaults to `process.env`. */
  env?: NodeJS.ProcessEnv;
};

type ResolvedOptions = { url: string; token: string; publicUrl?: string; defaultUser?: string };

function kumobaHome(env: NodeJS.ProcessEnv): string {
  return env.KUMOBA_HOME ?? join(homedir(), ".kumoba");
}

function resolveOptions(options: KumobaOptions): ResolvedOptions {
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
  const publicUrl = options.publicUrl ?? env.KUMOBA_PUBLIC_URL;
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
  private readonly baseUrl: string | null;

  /**
   * With no options, connects to the kumobad that `KUMOBA_URL` names or, failing that,
   * to a zero-config kumobad in `~/.kumoba`.
   */
  constructor(options: KumobaOptions = {}) {
    const resolved = resolveOptions(options);
    this.api = new Api(resolved.url, resolved.token);
    this.defaultUser = resolved.defaultUser;
    this.baseUrl = resolved.publicUrl ?? (resolved.url.startsWith("http://") ? resolved.url : null);
  }

  /** Same as `new Kumoba({ env })`: configure from `KUMOBA_*` variables with `~/.kumoba` defaults. */
  static fromEnv(env: NodeJS.ProcessEnv = process.env): Kumoba {
    return new Kumoba({ env });
  }

  private context(user: string | undefined): SandboxContext {
    return { api: this.api, defaultUser: user ?? this.defaultUser, baseUrl: this.baseUrl };
  }

  /** Names of the images kumobad can boot. */
  async images(): Promise<string[]> {
    return this.api.images();
  }

  /** Create a sandbox and, unless `start: false`, boot it. */
  async create(options: CreateOptions = {}): Promise<Sandbox> {
    const summary = await this.api.create({
      image: options.image ?? "default",
      labels: options.labels ?? {},
      cpu: options.cpu ?? 2,
      memoryMib: options.memoryMib ?? 4096,
      autoStopMinutes: options.autoStopMinutes ?? 15,
      autoDeleteMinutes: options.autoDeleteMinutes === undefined ? null : options.autoDeleteMinutes,
      network: toNetwork(options.network),
    });
    const sandbox = new Sandbox(this.context(options.user), toInfo(summary));
    if (options.start ?? true) {
      try {
        await sandbox.start();
      } catch (error) {
        await sandbox.delete().catch(() => undefined);
        throw error;
      }
    }
    return sandbox;
  }

  /** Reconnect to an existing sandbox by id. Throws a `KumobaError` with `notFound` when it is gone. */
  async get(id: string, options: { user?: string } = {}): Promise<Sandbox> {
    return new Sandbox(this.context(options.user), toInfo(await this.api.get(id)));
  }

  /** Like `get`, but resolves to `undefined` instead of throwing when the sandbox does not exist. */
  async find(id: string, options: { user?: string } = {}): Promise<Sandbox | undefined> {
    try {
      return await this.get(id, options);
    } catch (error) {
      if (error instanceof KumobaError && error.notFound) return undefined;
      throw error;
    }
  }

  /** Sandboxes whose labels include every entry of `labels`, newest first. */
  async list(options: ListOptions = {}): Promise<Sandbox[]> {
    const { sandboxes } = await this.api.list(options.labels ?? {}, options.limit ?? 100);
    return sandboxes.map((summary) => new Sandbox(this.context(undefined), toInfo(summary)));
  }

  /**
   * Every proxied connection attempt across all sandboxes in a time window, oldest
   * first. kumobad keeps 72 hours; pages past `limit` are fetched automatically.
   */
  async *accessLog(options: AccessLogOptions = {}): AsyncGenerator<AccessEntry, void, undefined> {
    const untilMs = (options.until ?? new Date()).getTime();
    const sinceMs = (options.since ?? new Date(untilMs - 2 * 3_600_000)).getTime();
    const limit = options.limit ?? 10_000;
    let cursor: string | undefined;
    for (;;) {
      const page = await this.api.accessLog({
        sinceMs,
        untilMs,
        limit,
        ...(cursor ? { cursor } : {}),
      });
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
  }

  /** Forget secrets by name across every sandbox. Returns the names that existed. */
  async deleteSecrets(names: readonly string[]): Promise<string[]> {
    return this.api.deleteSecrets(names);
  }
}
