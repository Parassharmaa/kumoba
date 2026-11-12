import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";

export type FakeSandbox = {
  id: string;
  image: string;
  labels: Record<string, string>;
  cpu: number;
  memoryMib: number;
  autoStopMinutes: number;
  autoDeleteMinutes: number | null;
  network: { mode: string; hosts: string[] };
  createdAtMs: number;
  lastActivityAtMs: number;
  state: "started" | "stopped" | "destroyed";
  files: Map<string, Buffer>;
  secrets: { name: string; envVars: string[] }[];
};

export type FakeExecCall = {
  sandboxId: string;
  argv: string[];
  cwd?: string;
  env?: Record<string, string>;
  timeoutMs: number;
  user?: string;
};

export type FakeExecReply =
  | { stdout?: string; stderr?: string; exitCode?: number; timedOut?: boolean; signal?: number }
  | { error: string };

export type FakeAccessEntry = {
  sandboxId: string;
  labels: Record<string, string>;
  host: string;
  port: number;
  decision: "allowed" | "denied" | "dropped";
  bytesIn: number;
  bytesOut: number;
  startedAtMs: number;
  completedAtMs: number;
  dropped?: number;
};

export type FakeKumobad = {
  url: string;
  token: string;
  sandboxes: Map<string, FakeSandbox>;
  accessEntries: FakeAccessEntry[];
  accessPageSize: number;
  execs: FakeExecCall[];
  onExec: (call: FakeExecCall) => FakeExecReply;
  close(): Promise<void>;
};

async function body(request: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    if (chunk instanceof Uint8Array) chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}

function send(response: ServerResponse, status: number, value: unknown): void {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}

function summary(sandbox: FakeSandbox) {
  const { files: _files, secrets: _secrets, ...rest } = sandbox;
  return rest;
}

export async function startFakeKumobad(): Promise<FakeKumobad> {
  const directory = mkdtempSync(path.join(tmpdir(), "fake-kumobad-"));
  const socketPath = path.join(directory, "kumobad.sock");
  let counter = 0;
  const fake: FakeKumobad = {
    url: `unix://${socketPath}`,
    token: "fake-token-0123456789abcdef0123456789",
    sandboxes: new Map(),
    accessEntries: [],
    accessPageSize: 1000,
    execs: [],
    onExec: () => ({ stdout: "", exitCode: 0 }),
    close: async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      rmSync(directory, { recursive: true, force: true });
    },
  };

  const server: Server = createServer((request, response) => {
    void (async () => {
      if (request.headers.authorization !== `Bearer ${fake.token}`) {
        send(response, 401, { error: "unauthorized" });
        return;
      }
      const url = new URL(request.url ?? "/", "http://fake");
      const parts = url.pathname.split("/").filter(Boolean);
      const method = request.method ?? "GET";
      const sandbox = parts[2] ? fake.sandboxes.get(parts[2]) : undefined;
      if (method === "GET" && url.pathname === "/v1/access-log") {
        const sinceMs = Number(url.searchParams.get("sinceMs"));
        const untilMs = Number(url.searchParams.get("untilMs"));
        const limit = Math.min(Number(url.searchParams.get("limit")), fake.accessPageSize);
        const matched = fake.accessEntries
          .filter((entry) => entry.completedAtMs >= sinceMs && entry.completedAtMs < untilMs)
          .sort((a, b) => a.completedAtMs - b.completedAtMs);
        send(response, 200, { entries: matched.slice(0, limit), truncated: matched.length > limit });
        return;
      }
      if (method === "GET" && url.pathname === "/v1/images") {
        send(response, 200, { images: ["default", "test"] });
        return;
      }
      if (method === "POST" && url.pathname === "/v1/sandboxes") {
        const input = JSON.parse((await body(request)).toString()) as Partial<FakeSandbox>;
        counter += 1;
        const created: FakeSandbox = {
          id: `sbx-${counter}`,
          image: input.image ?? "default",
          labels: input.labels ?? {},
          cpu: input.cpu ?? 2,
          memoryMib: input.memoryMib ?? 4096,
          autoStopMinutes: input.autoStopMinutes ?? 0,
          autoDeleteMinutes: input.autoDeleteMinutes ?? null,
          network: input.network ?? { mode: "open", hosts: [] },
          createdAtMs: Date.now() + counter,
          lastActivityAtMs: Date.now(),
          state: "stopped",
          files: new Map(),
          secrets: [],
        };
        fake.sandboxes.set(created.id, created);
        send(response, 200, summary(created));
        return;
      }
      if (method === "GET" && url.pathname === "/v1/sandboxes") {
        const filters = url.searchParams.getAll("label").map((pair) => pair.split(":"));
        const limit = Number(url.searchParams.get("limit") ?? 1000);
        const matched = [...fake.sandboxes.values()].filter((candidate) =>
          filters.every(([key, value]) => key !== undefined && candidate.labels[key] === value)
        );
        send(response, 200, {
          sandboxes: matched.slice(0, limit).map(summary),
          truncated: matched.length > limit,
        });
        return;
      }
      if (method === "POST" && url.pathname === "/v1/secrets/delete") {
        const { names } = JSON.parse((await body(request)).toString()) as { names: string[] };
        const deleted = new Set<string>();
        for (const candidate of fake.sandboxes.values()) {
          for (const secret of candidate.secrets)
            if (names.includes(secret.name)) deleted.add(secret.name);
          candidate.secrets = candidate.secrets.filter((secret) => !names.includes(secret.name));
        }
        send(response, 200, { deletedNames: [...deleted] });
        return;
      }
      if (!sandbox) {
        send(response, 404, { error: "not_found", message: "sandbox not found" });
        return;
      }
      const action = parts[3];
      if (method === "GET" && action === undefined) {
        send(response, 200, summary(sandbox));
      } else if (method === "PATCH" && action === undefined) {
        const patch = JSON.parse((await body(request)).toString()) as Partial<FakeSandbox>;
        if (patch.labels) sandbox.labels = patch.labels;
        if (patch.autoStopMinutes !== undefined) sandbox.autoStopMinutes = patch.autoStopMinutes;
        send(response, 200, summary(sandbox));
      } else if (method === "DELETE" && action === undefined) {
        fake.sandboxes.delete(sandbox.id);
        send(response, 200, { deleted: true });
      } else if (method === "POST" && (action === "start" || action === "stop")) {
        sandbox.state = action === "start" ? "started" : "stopped";
        send(response, 200, summary(sandbox));
      } else if (method === "PUT" && action === "secrets") {
        const input = JSON.parse((await body(request)).toString()) as {
          secrets: { name: string; envVars: string[] }[];
        };
        sandbox.secrets = input.secrets;
        send(response, 200, { mountedEnvVars: input.secrets.flatMap((secret) => secret.envVars) });
      } else if (method === "POST" && action === "preview") {
        const { port } = JSON.parse((await body(request)).toString()) as { port: number };
        send(response, 200, {
          path: `/v1/preview/token-${String(port)}/`,
          expiresAtMs: Date.now() + 60_000,
        });
      } else if (method === "POST" && action === "exec") {
        const call = JSON.parse((await body(request)).toString()) as Omit<
          FakeExecCall,
          "sandboxId"
        >;
        const exec = { sandboxId: sandbox.id, ...call };
        fake.execs.push(exec);
        const reply = fake.onExec(exec);
        response.writeHead(200, { "content-type": "application/x-ndjson" });
        if ("error" in reply) {
          response.end(`${JSON.stringify({ type: "error", message: reply.error })}\n`);
          return;
        }
        const lines = [
          ...(reply.stdout
            ? [{ type: "stdout", data: Buffer.from(reply.stdout).toString("base64") }]
            : []),
          ...(reply.stderr
            ? [{ type: "stderr", data: Buffer.from(reply.stderr).toString("base64") }]
            : []),
          {
            type: "exit",
            exitCode: reply.timedOut ? null : (reply.exitCode ?? 0),
            timedOut: reply.timedOut ?? false,
            ...(reply.signal !== undefined ? { signal: reply.signal } : {}),
          },
        ];
        response.end(lines.map((line) => `${JSON.stringify(line)}\n`).join(""));
      } else if (action === "files" && parts[4] === "list") {
        const directoryPath = url.searchParams.get("path") ?? "/";
        const prefix = `${directoryPath.replace(/\/$/, "")}/`;
        const names = new Map<string, { kind: string; size: number }>();
        for (const [filePath, content] of sandbox.files) {
          if (!filePath.startsWith(prefix)) continue;
          const [name, ...rest] = filePath.slice(prefix.length).split("/");
          if (name)
            names.set(
              name,
              rest.length > 0
                ? { kind: "directory", size: 0 }
                : { kind: "file", size: content.length }
            );
        }
        send(response, 200, {
          entries: [...names]
            .map(([name, entry]) => ({ name, ...entry }))
            .sort((a, b) => a.name.localeCompare(b.name)),
        });
      } else if (action === "files" && method === "PUT") {
        const filePath = url.searchParams.get("path") ?? "";
        sandbox.files.set(filePath, await body(request));
        send(response, 200, { written: sandbox.files.get(filePath)?.length });
      } else if (action === "files" && method === "GET") {
        const content = sandbox.files.get(url.searchParams.get("path") ?? "");
        if (!content) {
          send(response, 422, { error: "guest_error", message: "No such file or directory" });
          return;
        }
        response.writeHead(200, { "content-type": "application/octet-stream" });
        response.end(content);
      } else {
        send(response, 404, { error: "not_found", message: "route not found" });
      }
    })();
  });
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
  return fake;
}
