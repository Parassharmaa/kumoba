import type { IncomingMessage } from "node:http";
import { z } from "zod";
import { KumobaError } from "./errors.js";
import { type Body, Http, readBody } from "./http.js";
import type { Network, SandboxInfo, Secret } from "./types.js";

const networkSchema = z.object({
  mode: z.enum(["open", "allowlist", "blocked"]),
  hosts: z.array(z.string()),
});

const summarySchema = z.object({
  id: z.string(),
  image: z.string(),
  labels: z.record(z.string(), z.string()),
  cpu: z.number(),
  memoryMib: z.number(),
  autoStopMinutes: z.number(),
  autoDeleteMinutes: z.number().nullable(),
  network: networkSchema,
  createdAtMs: z.number(),
  lastActivityAtMs: z.number(),
  state: z.enum(["started", "stopped", "destroyed"]),
});

const listSchema = z.object({ sandboxes: z.array(summarySchema), truncated: z.boolean() });
const imagesSchema = z.object({ images: z.array(z.string()) });

const execEventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.enum(["stdout", "stderr"]), data: z.string() }),
  z.object({
    type: z.literal("exit"),
    exitCode: z.number().nullable(),
    timedOut: z.boolean(),
    signal: z.number().optional(),
  }),
  z.object({ type: z.literal("error"), message: z.string() }),
]);

const entriesSchema = z.object({
  entries: z.array(
    z.object({
      name: z.string(),
      kind: z.enum(["file", "directory", "other"]),
      size: z.number(),
    })
  ),
});

const previewSchema = z.object({ path: z.string(), expiresAtMs: z.number() });
const mountedSchema = z.object({ mountedEnvVars: z.array(z.string()) });
const deletedSchema = z.object({ deletedNames: z.array(z.string()) });
const anySchema = z.unknown();

export type Summary = z.infer<typeof summarySchema>;
export type ExecEvent = z.infer<typeof execEventSchema>;

export type CreateRequest = {
  image: string;
  labels: Record<string, string>;
  cpu: number;
  memoryMib: number;
  autoStopMinutes: number;
  autoDeleteMinutes: number | null;
  network: Network;
};

export type ExecRequest = {
  argv: string[];
  cwd?: string;
  env?: Record<string, string>;
  timeoutMs: number;
  user?: string;
};

const MAX_EXEC_LINE = 1024 * 1024;

export function toInfo(summary: Summary): SandboxInfo {
  const { createdAtMs, lastActivityAtMs, ...rest } = summary;
  return { ...rest, createdAt: new Date(createdAtMs), lastActivityAt: new Date(lastActivityAtMs) };
}

function sandboxPath(id: string, suffix = "", query?: URLSearchParams): string {
  const base = `/v1/sandboxes/${encodeURIComponent(id)}${suffix}`;
  return query ? `${base}?${query.toString()}` : base;
}

function fileQuery(path: string, user: string | undefined): URLSearchParams {
  const query = new URLSearchParams({ path });
  if (user) query.set("user", user);
  return query;
}

/** Thin, typed wrapper over every kumobad route. `Kumoba` and `Sandbox` build on it. */
export class Api {
  readonly http: Http;

  constructor(url: string, token: string) {
    this.http = new Http(url, token);
  }

  async images(): Promise<string[]> {
    return (await this.http.json(imagesSchema, { method: "GET", path: "/v1/images" })).images;
  }

  async create(request: CreateRequest): Promise<Summary> {
    return this.http.json(summarySchema, { method: "POST", path: "/v1/sandboxes", json: request });
  }

  async get(id: string): Promise<Summary> {
    return this.http.json(summarySchema, { method: "GET", path: sandboxPath(id) });
  }

  async list(
    labels: Record<string, string>,
    limit: number
  ): Promise<{ sandboxes: Summary[]; truncated: boolean }> {
    const query = new URLSearchParams({ limit: String(limit) });
    for (const [key, value] of Object.entries(labels)) query.append("label", `${key}:${value}`);
    return this.http.json(listSchema, { method: "GET", path: `/v1/sandboxes?${query.toString()}` });
  }

  async update(
    id: string,
    patch: { labels?: Record<string, string>; autoStopMinutes?: number }
  ): Promise<Summary> {
    return this.http.json(summarySchema, { method: "PATCH", path: sandboxPath(id), json: patch });
  }

  async start(id: string): Promise<Summary> {
    return this.http.json(summarySchema, { method: "POST", path: sandboxPath(id, "/start") });
  }

  async stop(id: string): Promise<Summary> {
    return this.http.json(summarySchema, { method: "POST", path: sandboxPath(id, "/stop") });
  }

  async delete(id: string): Promise<void> {
    await this.http.json(anySchema, { method: "DELETE", path: sandboxPath(id) });
  }

  async preview(
    id: string,
    port: number,
    ttlSeconds: number
  ): Promise<{ path: string; expiresAtMs: number }> {
    return this.http.json(previewSchema, {
      method: "POST",
      path: sandboxPath(id, "/preview"),
      json: { port, ttlSeconds },
    });
  }

  async putSecrets(id: string, secrets: readonly Secret[]): Promise<string[]> {
    const result = await this.http.json(mountedSchema, {
      method: "PUT",
      path: sandboxPath(id, "/secrets"),
      json: { secrets },
    });
    return result.mountedEnvVars;
  }

  async deleteSecrets(names: readonly string[]): Promise<string[]> {
    const result = await this.http.json(deletedSchema, {
      method: "POST",
      path: "/v1/secrets/delete",
      json: { names },
    });
    return result.deletedNames;
  }

  async exec(
    id: string,
    request: ExecRequest,
    onEvent: (event: ExecEvent) => void,
    signal?: AbortSignal
  ): Promise<void> {
    const response = await this.http.open({
      method: "POST",
      path: sandboxPath(id, "/exec"),
      json: request,
      ...(signal ? { signal } : {}),
    });
    let pending = "";
    const decoder = new TextDecoder();
    for await (const chunk of response) {
      if (!(chunk instanceof Uint8Array)) continue;
      pending += decoder.decode(chunk, { stream: true });
      if (pending.length > MAX_EXEC_LINE) {
        response.destroy();
        throw new KumobaError("kumobad exec event exceeds limit", {
          status: 502,
          code: "protocol",
        });
      }
      let newline = pending.indexOf("\n");
      while (newline >= 0) {
        const line = pending.slice(0, newline);
        pending = pending.slice(newline + 1);
        if (line.length > 0) onEvent(parseEvent(line));
        newline = pending.indexOf("\n");
      }
    }
  }

  async upload(id: string, path: string, body: Body, user: string | undefined): Promise<void> {
    const response = await this.http.open({
      method: "PUT",
      path: sandboxPath(id, "/files", fileQuery(path, user)),
      body,
    });
    await readBody(response);
  }

  async download(id: string, path: string, user: string | undefined): Promise<IncomingMessage> {
    return this.http.open({
      method: "GET",
      path: sandboxPath(id, "/files", fileQuery(path, user)),
    });
  }

  async listFiles(id: string, path: string, user: string | undefined) {
    const result = await this.http.json(entriesSchema, {
      method: "GET",
      path: sandboxPath(id, "/files/list", fileQuery(path, user)),
    });
    return result.entries;
  }
}

function parseEvent(line: string): ExecEvent {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    throw new KumobaError("kumobad sent a malformed exec event", { status: 502, code: "protocol" });
  }
  const parsed = execEventSchema.safeParse(value);
  if (!parsed.success) {
    throw new KumobaError("kumobad sent an unexpected exec event", {
      status: 502,
      code: "protocol",
      cause: parsed.error,
    });
  }
  return parsed.data;
}
