import { readFileSync } from "node:fs";
import { Api, toInfo } from "./api.js";
import { KumobaError } from "./errors.js";
import { Sandbox, type SandboxContext } from "./sandbox.js";
import type { CreateOptions, ListOptions, Network, NetworkOption } from "./types.js";

export type KumobaOptions = {
  /** `unix:///path/to/kumobad.sock` or `http://127.0.0.1:<port>`. */
  url: string;
  /** Bearer token, the contents of the daemon's `KUMOBAD_TOKEN_FILE`. */
  token: string;
  /**
   * Base URL browsers can reach kumobad through, used to build `expose()` links.
   * Defaults to the `http://` URL when `url` is one, otherwise links carry only a path.
   */
  publicUrl?: string;
  /** Guest user that commands and file operations act as. Per-sandbox `user` overrides it. */
  defaultUser?: string;
};

function toNetwork(option: NetworkOption | undefined): Network {
  if (option === undefined || option === "open") return { mode: "open", hosts: [] };
  if (option === "blocked") return { mode: "blocked", hosts: [] };
  return { mode: "allowlist", hosts: option.allow };
}

/** Entry point. Talks to one kumobad daemon. */
export class Kumoba {
  private readonly api: Api;
  private readonly defaultUser: string | undefined;
  private readonly baseUrl: string | null;

  constructor(options: KumobaOptions) {
    this.api = new Api(options.url, options.token);
    this.defaultUser = options.defaultUser;
    this.baseUrl = options.publicUrl ?? (options.url.startsWith("http://") ? options.url : null);
  }

  /**
   * Configure from the environment:
   * `KUMOBA_URL`, `KUMOBA_TOKEN` or `KUMOBA_TOKEN_FILE`, and optional
   * `KUMOBA_PUBLIC_URL` and `KUMOBA_USER`.
   */
  static fromEnv(env: NodeJS.ProcessEnv = process.env): Kumoba {
    const url = env.KUMOBA_URL;
    if (!url) throw new KumobaError("KUMOBA_URL is not set", { code: "config_invalid" });
    let token = env.KUMOBA_TOKEN;
    if (!token && env.KUMOBA_TOKEN_FILE) token = readFileSync(env.KUMOBA_TOKEN_FILE, "utf8").trim();
    if (!token)
      throw new KumobaError("Set KUMOBA_TOKEN or KUMOBA_TOKEN_FILE", { code: "config_invalid" });
    return new Kumoba({
      url,
      token,
      ...(env.KUMOBA_PUBLIC_URL ? { publicUrl: env.KUMOBA_PUBLIC_URL } : {}),
      ...(env.KUMOBA_USER ? { defaultUser: env.KUMOBA_USER } : {}),
    });
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

  /** Forget secrets by name across every sandbox. Returns the names that existed. */
  async deleteSecrets(names: readonly string[]): Promise<string[]> {
    return this.api.deleteSecrets(names);
  }
}
