import type { IncomingMessage } from "node:http";
import type { Readable } from "node:stream";
import { z } from "zod";
import { KumobaError } from "./errors.js";
import { type Body, checkedDownload, Http } from "./http.js";
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
  z.object({ type: z.literal("sandbox"), id: z.string() }),
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

const accessEntrySchema = z.object({
  sandboxId: z.string(),
  labels: z.record(z.string(), z.string()),
  host: z.string(),
  port: z.number(),
  decision: z.string(),
  bytesIn: z.number(),
  bytesOut: z.number(),
  startedAtMs: z.number(),
  completedAtMs: z.number(),
  dropped: z.number().optional(),
});
const accessLogSchema = z.object({
  entries: z.array(accessEntrySchema),
  truncated: z.boolean(),
  nextCursor: z.string().nullable().optional(),
});

const previewSchema = z.object({
  url: z.string().nullish(),
  path: z.string(),
  expiresAtMs: z.number(),
});
const desktopSchema = previewSchema.extend({ viewer: z.enum(["stream", "vnc"]) });
const mountedSchema = z.object({ mountedEnvVars: z.array(z.string()) });
const deletedSchema = z.object({ deletedNames: z.array(z.string()) });
const anySchema = z.unknown();

export type Summary = z.infer<typeof summarySchema>;
export type AccessEntrySummary = z.infer<typeof accessEntrySchema>;
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

export type RunRequest = ExecRequest & {
  image?: string;
  cpu?: number;
  memoryMib?: number;
  labels?: Record<string, string>;
  network?: Network;
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

  async images(signal?: AbortSignal): Promise<string[]> {
    return (await this.http.json(imagesSchema, { method: "GET", path: "/v1/images", signal }))
      .images;
  }

  async create(request: CreateRequest, signal?: AbortSignal): Promise<Summary> {
    return this.http.json(summarySchema, {
      method: "POST",
      path: "/v1/sandboxes",
      json: request,
      signal,
    });
  }

  async get(id: string, signal?: AbortSignal): Promise<Summary> {
    return this.http.json(summarySchema, { method: "GET", path: sandboxPath(id), signal });
  }

  async list(
    labels: Record<string, string>,
    limit: number | undefined,
    signal?: AbortSignal
  ): Promise<{ sandboxes: Summary[]; truncated: boolean }> {
    const query = new URLSearchParams();
    if (limit !== undefined) {
      if (!Number.isSafeInteger(limit) || limit < 0) {
        throw new KumobaError(`limit must be a whole number of 0 or more, got ${String(limit)}`, {
          code: "invalid_request",
        });
      }
      query.set("limit", String(limit));
    }
    for (const [key, value] of Object.entries(labels)) {
      if (key === "" || key.includes(":")) {
        throw new KumobaError(
          `label filter keys must be non-empty and contain no ":", got ${JSON.stringify(key)}`,
          { code: "invalid_request" }
        );
      }
      query.append("label", `${key}:${value}`);
    }
    return this.http.json(listSchema, {
      method: "GET",
      path: `/v1/sandboxes?${query.toString()}`,
      signal,
    });
  }

  async update(
    id: string,
    patch: { labels?: Record<string, string>; autoStopMinutes?: number; network?: Network },
    signal?: AbortSignal
  ): Promise<Summary> {
    return this.http.json(summarySchema, {
      method: "PATCH",
      path: sandboxPath(id),
      json: patch,
      signal,
    });
  }

  async start(id: string, signal?: AbortSignal): Promise<Summary> {
    return this.http.json(summarySchema, {
      method: "POST",
      path: sandboxPath(id, "/start"),
      signal,
    });
  }

  async stop(id: string, signal?: AbortSignal): Promise<Summary> {
    return this.http.json(summarySchema, {
      method: "POST",
      path: sandboxPath(id, "/stop"),
      signal,
    });
  }

  async delete(id: string, signal?: AbortSignal): Promise<void> {
    await this.http.json(anySchema, { method: "DELETE", path: sandboxPath(id), signal });
  }

  async accessLog(
    query: {
      sinceMs: number;
      untilMs: number;
      limit: number;
      cursor?: string;
    },
    signal?: AbortSignal
  ): Promise<{
    entries: AccessEntrySummary[];
    truncated: boolean;
    nextCursor?: string | null | undefined;
  }> {
    const params = new URLSearchParams({
      sinceMs: String(query.sinceMs),
      untilMs: String(query.untilMs),
      limit: String(query.limit),
    });
    if (query.cursor) params.set("cursor", query.cursor);
    return this.http.json(accessLogSchema, {
      method: "GET",
      path: `/v1/access-log?${params.toString()}`,
      signal,
    });
  }

  async preview(
    id: string,
    port: number,
    ttlSeconds: number,
    signal?: AbortSignal
  ): Promise<{ path: string; expiresAtMs: number }> {
    return this.http.json(previewSchema, {
      method: "POST",
      path: sandboxPath(id, "/preview"),
      json: { port, ttlSeconds },
      signal,
    });
  }

  async desktop(
    id: string,
    ttlSeconds: number,
    viewer?: "stream" | "vnc",
    signal?: AbortSignal
  ): Promise<{ path: string; expiresAtMs: number; viewer: "stream" | "vnc" }> {
    return this.http.json(desktopSchema, {
      method: "POST",
      path: sandboxPath(id, "/desktop"),
      json: { ttlSeconds, ...(viewer === undefined ? {} : { viewer }) },
      signal,
    });
  }

  async putSecrets(
    id: string,
    secrets: readonly Secret[],
    signal?: AbortSignal
  ): Promise<string[]> {
    const result = await this.http.json(mountedSchema, {
      method: "PUT",
      path: sandboxPath(id, "/secrets"),
      json: { secrets },
      signal,
    });
    return result.mountedEnvVars;
  }

  async deleteSecrets(names: readonly string[], signal?: AbortSignal): Promise<string[]> {
    const result = await this.http.json(deletedSchema, {
      method: "POST",
      path: "/v1/secrets/delete",
      json: { names },
      signal,
    });
    return result.deletedNames;
  }

  async exec(
    id: string,
    request: ExecRequest,
    onEvent: (event: ExecEvent) => void,
    signal?: AbortSignal
  ): Promise<void> {
    await this.events(sandboxPath(id, "/exec"), request, onEvent, signal);
  }

  async run(
    request: RunRequest,
    onEvent: (event: ExecEvent) => void,
    signal?: AbortSignal
  ): Promise<void> {
    await this.events("/v1/run", request, onEvent, signal);
  }

  private async events(
    path: string,
    request: ExecRequest | RunRequest,
    onEvent: (event: ExecEvent) => void,
    signal?: AbortSignal
  ): Promise<void> {
    const response = await this.http.open({ method: "POST", path, json: request, signal });
    for await (const line of this.lines(response, signal)) onEvent(parseEvent(line));
  }

  private async *lines(response: IncomingMessage, signal?: AbortSignal): AsyncGenerator<string> {
    let pending = "";
    const decoder = new TextDecoder();
    try {
      for await (const chunk of response) {
        if (!(chunk instanceof Uint8Array)) continue;
        pending += decoder.decode(chunk, { stream: true });
        if (pending.length > MAX_EXEC_LINE) {
          throw new KumobaError("kumobad exec event exceeds limit", {
            status: 502,
            code: "protocol",
          });
        }
        let newline = pending.indexOf("\n");
        while (newline >= 0) {
          const line = pending.slice(0, newline);
          pending = pending.slice(newline + 1);
          if (line.length > 0) yield line;
          newline = pending.indexOf("\n");
        }
      }
    } catch (cause) {
      throw this.http.failure(cause, signal, "lost");
    }
  }

  async upload(
    id: string,
    path: string,
    body: Body,
    user: string | undefined,
    signal?: AbortSignal
  ): Promise<void> {
    const response = await this.http.open({
      method: "PUT",
      path: sandboxPath(id, "/files", fileQuery(path, user)),
      body,
      signal,
    });
    await this.http.body(response, signal);
  }

  async download(
    id: string,
    path: string,
    user: string | undefined,
    signal?: AbortSignal
  ): Promise<Readable> {
    const response = await this.http.open({
      method: "GET",
      path: sandboxPath(id, "/files", fileQuery(path, user)),
      signal,
    });
    return checkedDownload(response, signal);
  }

  async listFiles(id: string, path: string, user: string | undefined, signal?: AbortSignal) {
    const result = await this.http.json(entriesSchema, {
      method: "GET",
      path: sandboxPath(id, "/files/list", fileQuery(path, user)),
      signal,
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
