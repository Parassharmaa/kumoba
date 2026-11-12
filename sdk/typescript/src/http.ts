import { request as httpRequest, type IncomingMessage, type RequestOptions } from "node:http";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { z } from "zod";
import { KumobaError } from "./errors.js";

export type Endpoint = { socketPath: string } | { host: string; port: number };

export type Body = AsyncIterable<Uint8Array> | Iterable<Uint8Array>;

export type HttpInit = {
  method: string;
  path: string;
  json?: unknown;
  body?: Body;
  signal?: AbortSignal;
};

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
      outgoing.on("error", (cause) => reject(wrapTransportError(cause, this.endpoint)));
      if (init.body) {
        pipeline(Readable.from(init.body), outgoing).catch(reject);
      } else {
        outgoing.end(payload);
      }
    });
    const status = response.statusCode ?? 500;
    if (status < 400) return response;
    const text = (await readBody(response)).toString("utf8");
    const parsed = errorSchema.safeParse(safeJson(text));
    const message = parsed.success
      ? (parsed.data.message ?? parsed.data.error ?? text)
      : text || `kumobad responded ${String(status)}`;
    const code = parsed.success ? (parsed.data.error ?? "daemon_error") : "daemon_error";
    throw new KumobaError(message, { status, code, retryable: status === 429 || status >= 500 });
  }

  async json<T>(schema: z.ZodType<T>, init: HttpInit): Promise<T> {
    const response = await this.open(init);
    const text = (await readBody(response)).toString("utf8");
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
}

function wrapTransportError(cause: unknown, endpoint: Endpoint): KumobaError {
  const where =
    "socketPath" in endpoint ? endpoint.socketPath : `${endpoint.host}:${String(endpoint.port)}`;
  const detail = cause instanceof Error ? cause.message : String(cause);
  return new KumobaError(`Cannot reach kumobad at ${where}: ${detail}`, {
    status: 503,
    code: "daemon_error",
    retryable: true,
    cause,
  });
}
