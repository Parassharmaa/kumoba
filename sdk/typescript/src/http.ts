import { request as httpRequest, type IncomingMessage, type RequestOptions } from "node:http";
import { type Duplex, Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { z } from "zod";
import { KumobaError } from "./errors.js";
import type { CallOptions } from "./types.js";

export type Endpoint = { socketPath: string } | { host: string; port: number };

export type Body = AsyncIterable<Uint8Array> | Iterable<Uint8Array>;

export type HttpInit = {
  method: string;
  path: string;
  json?: unknown;
  body?: Body;
  signal?: AbortSignal | undefined;
};

const MAX_TIMEOUT_MS = 2 ** 31 - 1;
export const CLEANUP_TIMEOUT_MS = 10_000;

class Timeout {
  constructor(readonly ms: number) {}
}

export type Deadline = { signal: AbortSignal | undefined; clear(): void };

export function deadline(options: CallOptions | undefined): Deadline {
  const signal = options?.signal;
  const timeoutMs = options?.timeoutMs;
  if (timeoutMs === undefined) return { signal, clear: () => undefined };
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > MAX_TIMEOUT_MS) {
    throw new KumobaError(
      `timeoutMs must be a number of milliseconds above 0 and up to ${String(MAX_TIMEOUT_MS)}, got ${String(timeoutMs)}`,
      { code: "invalid_request" }
    );
  }
  const timer = new AbortController();
  const handle = setTimeout(() => timer.abort(new Timeout(timeoutMs)), timeoutMs);
  handle.unref();
  return {
    signal: signal ? AbortSignal.any([signal, timer.signal]) : timer.signal,
    clear: () => clearTimeout(handle),
  };
}

export async function withDeadline<T>(
  options: CallOptions | undefined,
  call: (signal: AbortSignal | undefined) => Promise<T>
): Promise<T> {
  const limit = deadline(options);
  try {
    return await call(limit.signal);
  } finally {
    limit.clear();
  }
}

const MAX_JSON_BYTES = 16 * 1024 * 1024;

const errorSchema = z.object({ error: z.string().optional(), message: z.string().optional() });

export function parseEndpoint(url: string): Endpoint {
  if (url.startsWith("unix://")) return { socketPath: url.slice("unix://".length) };
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new KumobaError(`Invalid kumobad URL: ${url}`, { code: "config_invalid" });
  }
  if (parsed.protocol !== "http:") {
    throw new KumobaError(
      "kumobad URL must be unix:///path/to/kumobad.sock or http://127.0.0.1:port",
      {
        code: "config_invalid",
      }
    );
  }
  return { host: parsed.hostname, port: Number(parsed.port || 80) };
}

export async function readBody(
  stream: AsyncIterable<unknown>,
  maxBytes = MAX_JSON_BYTES
): Promise<Buffer> {
  const chunks: Uint8Array[] = [];
  let total = 0;
  for await (const chunk of stream) {
    if (!(chunk instanceof Uint8Array)) {
      throw new KumobaError("kumobad returned a non-binary chunk", {
        status: 502,
        code: "protocol",
        retryable: true,
      });
    }
    total += chunk.byteLength;
    if (total > maxBytes) {
      throw new KumobaError(`kumobad response exceeds ${String(maxBytes)} bytes`, {
        status: 413,
        code: "response_too_large",
      });
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function downloadBroken(received: number, declared: number | undefined, cause?: unknown) {
  const of = declared === undefined ? "" : ` of ${String(declared)}`;
  return new KumobaError(`kumobad download ended after ${String(received)}${of} bytes`, {
    status: 502,
    code: "protocol",
    retryable: true,
    ...(cause === undefined ? {} : { cause }),
  });
}

export function checkedDownload(response: IncomingMessage, signal?: AbortSignal): Readable {
  const header = response.headers["x-file-size"];
  const declared = typeof header === "string" && /^\d+$/.test(header) ? Number(header) : undefined;
  let received = 0;
  const counter = new Transform({
    transform(chunk: Buffer, _encoding, done) {
      received += chunk.byteLength;
      done(null, chunk);
    },
    flush(done) {
      done(
        declared !== undefined && received < declared ? downloadBroken(received, declared) : null
      );
    },
  });
  response.on("error", (cause) =>
    counter.destroy(
      signal?.aborted ? abortedError(signal) : downloadBroken(received, declared, cause)
    )
  );
  counter.on("close", () => {
    if (!response.complete) response.destroy();
  });
  return response.pipe(counter);
}

function safeJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

export class Http {
  readonly endpoint: Endpoint;

  constructor(
    url: string,
    private readonly token: string
  ) {
    this.endpoint = parseEndpoint(url);
  }

  async open(init: HttpInit): Promise<IncomingMessage> {
    if (init.signal?.aborted) throw abortedError(init.signal);
    const headers: Record<string, string> = { authorization: `Bearer ${this.token}` };
    const payload = init.json === undefined ? undefined : Buffer.from(JSON.stringify(init.json));
    if (payload) {
      headers["content-type"] = "application/json";
      headers["content-length"] = String(payload.length);
    }
    const options: RequestOptions = {
      ...this.endpoint,
      method: init.method,
      path: init.path,
      headers,
      ...(init.signal ? { signal: init.signal } : {}),
    };
    const response = await new Promise<IncomingMessage>((resolve, reject) => {
      const outgoing = httpRequest(options, resolve);
      outgoing.on("error", (cause) => reject(this.failure(cause, init.signal)));
      if (init.body) {
        pipeline(Readable.from(init.body), outgoing).catch((cause: unknown) => {
          reject(init.signal?.aborted ? abortedError(init.signal) : cause);
        });
      } else {
        outgoing.end(payload);
      }
    });
    const status = response.statusCode ?? 500;
    if (status < 400) return response;
    throw responseError(status, await this.body(response, init.signal));
  }

  failure(cause: unknown, signal: AbortSignal | undefined, stage: "connect" | "lost" = "connect") {
    if (signal?.aborted) return abortedError(signal);
    if (cause instanceof KumobaError) return cause;
    return wrapTransportError(cause, this.endpoint, stage);
  }

  async upgrade(
    path: string,
    json: unknown,
    protocol: string
  ): Promise<{ socket: Duplex; head: Buffer }> {
    const payload = Buffer.from(JSON.stringify(json));
    return new Promise((resolve, reject) => {
      const outgoing = httpRequest({
        ...this.endpoint,
        method: "POST",
        path,
        headers: {
          authorization: `Bearer ${this.token}`,
          "content-type": "application/json",
          "content-length": String(payload.length),
          connection: "Upgrade",
          upgrade: protocol,
        },
      });
      outgoing.on("upgrade", (_response, socket, head) => resolve({ socket, head }));
      outgoing.on("response", (response) => {
        this.body(response).then(
          (body) => reject(responseError(response.statusCode ?? 500, body)),
          reject
        );
      });
      outgoing.on("error", (cause) => reject(wrapTransportError(cause, this.endpoint)));
      outgoing.end(payload);
    });
  }

  async json<T>(schema: z.ZodType<T>, init: HttpInit): Promise<T> {
    const response = await this.open(init);
    const text = (await this.body(response, init.signal)).toString("utf8");
    const result = schema.safeParse(safeJson(text));
    if (!result.success) {
      throw new KumobaError(`kumobad returned an unexpected ${init.method} ${init.path} response`, {
        status: 502,
        code: "protocol",
        cause: result.error,
      });
    }
    return result.data;
  }

  async body(response: IncomingMessage, signal?: AbortSignal): Promise<Buffer> {
    try {
      return await readBody(response);
    } catch (cause) {
      throw this.failure(cause, signal, "lost");
    }
  }
}

export function abortedError(signal: AbortSignal): KumobaError {
  const reason: unknown = signal.reason;
  if (reason instanceof Timeout) {
    return new KumobaError(`kumoba call timed out after ${String(reason.ms)} ms`, {
      code: "timeout",
      retryable: true,
    });
  }
  return new KumobaError("kumoba request was aborted", {
    code: "aborted",
    retryable: false,
    cause: signal.reason,
  });
}

function responseError(status: number, body: Buffer): KumobaError {
  const text = body.toString("utf8");
  const parsed = errorSchema.safeParse(safeJson(text));
  const message = parsed.success
    ? (parsed.data.message ?? parsed.data.error ?? text)
    : text || `kumobad responded ${String(status)}`;
  const code = parsed.success ? (parsed.data.error ?? "daemon_error") : "daemon_error";
  return new KumobaError(message, { status, code, retryable: status === 429 || status >= 500 });
}

function wrapTransportError(
  cause: unknown,
  endpoint: Endpoint,
  stage: "connect" | "lost" = "connect"
): KumobaError {
  const where =
    "socketPath" in endpoint ? endpoint.socketPath : `${endpoint.host}:${String(endpoint.port)}`;
  const detail = cause instanceof Error ? cause.message : String(cause);
  const lead = stage === "connect" ? "Cannot reach" : "Lost the connection to";
  return new KumobaError(`${lead} kumobad at ${where}: ${detail}`, {
    status: 503,
    code: "unreachable",
    retryable: true,
    cause,
  });
}
