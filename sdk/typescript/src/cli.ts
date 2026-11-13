#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import type { IncomingMessage } from "node:http";
import { fileURLToPath } from "node:url";
import {
  bytesField,
  type Catalog,
  type CommandSpec,
  type JsonSchema,
  kebab,
  missing,
  type Parsed,
  parseArgs,
  types,
  UsageError,
} from "./cli-args.js";
import { KumobaError } from "./errors.js";
import { Http, readBody } from "./http.js";
import { resolveOptions } from "./kumoba.js";

const VERSION: string = (
  JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as {
    version: string;
  }
).version;

const LOCAL_HELP = `Local commands:
  help [command]       this page, or one command's arguments
  commands [--json]    every command the daemon serves; --json prints the full schemas
  version              this CLI's version and the daemon's

Arguments are positional values, key=value pairs or --kebab-case flags, coerced through
each command's schema. key=@file reads a value from a file, key=@- from stdin, and @@
escapes a literal @. Lists take a,b or repeated flags; maps take repeated key=value.
Everything after -- goes to the command's trailing list, such as the program to run.
--args '{...}' passes the arguments as one JSON object, -q prints only ids, --json prints
compact JSON, -o FILE writes binary output to a file, --open opens a returned url in the
browser (desktop live views and exposed ports).

Connection: KUMOBA_URL (default unix://$KUMOBA_HOME/kumobad.sock), KUMOBA_TOKEN or
KUMOBA_TOKEN_FILE (default $KUMOBA_HOME/token), KUMOBA_HOME (default ~/.kumoba).

Exit status: the guest command's own for exec and run, 1 when kumobad rejects a request,
2 for a usage error, 3 when kumobad cannot be reached.`;

export type Io = {
  env: NodeJS.ProcessEnv;
  stdout: (chunk: string | Uint8Array) => void;
  stderr: (chunk: string | Uint8Array) => void;
  stdin: () => Buffer;
  stdinIsTty: boolean;
  stdoutIsTty: boolean;
  open: (url: string) => void;
};

class Exit extends Error {
  constructor(
    readonly code: number,
    message: string
  ) {
    super(message);
  }
}

function exitFor(error: unknown): Exit {
  if (error instanceof Exit) return error;
  if (error instanceof UsageError) return new Exit(2, error.message);
  if (error instanceof KumobaError) {
    const unreachable = error.code === "unreachable" || error.code === "config_invalid";
    const suffix =
      error.code === "unreachable"
        ? ". Start it with `kumobad`, or point KUMOBA_URL at a running daemon"
        : error.status && !unreachable
          ? ` (HTTP ${String(error.status)})`
          : "";
    return new Exit(unreachable ? 3 : 1, `${error.message}${suffix}`);
  }
  return new Exit(1, error instanceof Error ? error.message : String(error));
}

async function fetchCatalog(http: Http, url: string): Promise<Catalog> {
  try {
    const response = await http.open({ method: "GET", path: "/v1/commands" });
    return JSON.parse((await readBody(response)).toString("utf8")) as Catalog;
  } catch (error) {
    if (error instanceof KumobaError && error.status === 404) {
      throw new Exit(
        3,
        `kumobad at ${url} does not describe its commands; upgrade it to ${VERSION} or later`
      );
    }
    throw error;
  }
}

function overview(io: Io, catalog: Catalog | undefined, url: string, problem?: string): void {
  const lines = [
    `kumoba ${VERSION}: command line for a kumobad sandbox daemon`,
    "",
    "Usage: kumoba <command> [arguments]   kumoba <command> --help",
    "",
  ];
  if (catalog) {
    lines.push(`Commands served by kumobad ${catalog.version} at ${url}:`);
    for (const command of catalog.commands) {
      if (command.name !== "commands")
        lines.push(`  ${command.name.padEnd(16)} ${command.summary}`);
    }
  } else {
    lines.push(
      `The command list comes from the daemon, which is not reachable now:\n  ${problem ?? ""}\n`
    );
  }
  lines.push("", LOCAL_HELP);
  io.stdout(`${lines.join("\n")}\n`);
}

function typeLabel(schema: JsonSchema): string {
  if (schema.enum) return schema.enum.map(String).join("|");
  const kind = types(schema).find((value) => value !== "null") ?? "string";
  if (kind === "array") return `${typeLabel(schema.items ?? {})}[]`;
  if (kind === "object" && !schema.properties) return "key=value";
  return kind;
}

function rows(
  schema: JsonSchema,
  prefix: string,
  required: string[],
  rest: string | null,
  out: [string, string, string][]
): void {
  for (const [name, property] of Object.entries(schema.properties ?? {}).sort(([a], [b]) =>
    a.localeCompare(b)
  )) {
    const dotted = `${prefix}${kebab(name)}`;
    if (property.properties) {
      rows(property, `${dotted}.`, [], null, out);
      continue;
    }
    let note = property.description ?? "";
    const fallback = property.default;
    const empty =
      fallback === undefined ||
      fallback === null ||
      (Array.isArray(fallback) && fallback.length === 0) ||
      (typeof fallback === "object" &&
        !Array.isArray(fallback) &&
        Object.keys(fallback).length === 0);
    if (!empty) note = `${note} (default ${JSON.stringify(fallback)})`;
    if (rest === name) note = `${note}; or everything after --`;
    out.push([
      `--${dotted} <${typeLabel(property)}>`,
      required.includes(name) ? "required" : "optional",
      note,
    ]);
  }
}

function commandHelp(io: Io, spec: CommandSpec): void {
  let usage = `kumoba ${spec.name}`;
  for (const name of spec.positional) usage += ` <${name}>`;
  usage += " [--flag value | key=value ...]";
  if (spec.rest) usage += ` -- <${kebab(spec.rest)}...>`;
  const lines = [`kumoba ${spec.name}: ${spec.summary}`, "", "Usage", `  ${usage}`, ""];
  const table: [string, string, string][] = [];
  rows(spec.inputSchema, "", spec.inputSchema.required ?? [], spec.rest, table);
  if (table.length > 0) {
    const width = Math.max(...table.map(([flag]) => flag.length));
    lines.push("Arguments");
    for (const [flag, status, note] of table) {
      lines.push(`  ${flag.padEnd(width)}  ${status.padEnd(8)}  ${note}`);
    }
  }
  const output =
    spec.output === "events"
      ? "the command's stdout and stderr, and its exit status"
      : spec.output === "bytes"
        ? "the raw bytes, on stdout or into -o FILE"
        : "JSON";
  lines.push("", `Prints ${output}. ${spec.method} ${spec.path}`);
  io.stdout(`${lines.join("\n")}\n`);
}

function ids(value: unknown): string[] {
  if (!value || typeof value !== "object") return [];
  const record = value as Record<string, unknown>;
  if (typeof record.id === "string") return [record.id];
  return Object.values(record)
    .filter(Array.isArray)
    .flat()
    .map((item) => (item && typeof item === "object" ? (item as { id?: unknown }).id : undefined))
    .filter((id): id is string => typeof id === "string");
}

async function streamEvents(io: Io, response: IncomingMessage): Promise<number> {
  let buffer = "";
  for await (const chunk of response) {
    buffer += (chunk as Buffer).toString("utf8");
    let end = buffer.indexOf("\n");
    while (end >= 0) {
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      end = buffer.indexOf("\n");
      let event: Record<string, unknown>;
      try {
        event = JSON.parse(line) as Record<string, unknown>;
      } catch {
        continue;
      }
      if (event.type === "stdout") io.stdout(Buffer.from(String(event.data), "base64"));
      else if (event.type === "stderr") io.stderr(Buffer.from(String(event.data), "base64"));
      else if (event.type === "exit") {
        if (event.timedOut === true) {
          io.stderr("kumoba: command timed out\n");
          return 124;
        }
        return typeof event.exitCode === "number" ? event.exitCode : 1;
      } else if (event.type === "error") throw new Exit(1, String(event.message));
    }
  }
  throw new Exit(1, "output stream ended before the command exited");
}

async function invoke(io: Io, http: Http, spec: CommandSpec, tokens: string[]): Promise<number> {
  const parsed: Parsed = parseArgs(spec, tokens, io.stdin);
  if (parsed.help) {
    commandHelp(io, spec);
    return 0;
  }
  if (bytesField(spec) && parsed.bytes === undefined && !io.stdinIsTty) parsed.bytes = io.stdin();
  const absent = missing(spec, parsed);
  if (absent.length > 0) {
    throw new UsageError(
      `missing ${absent.map(kebab).join(", ")}; \`kumoba ${spec.name} --help\` shows the arguments`
    );
  }
  let path = spec.path;
  const query = new URLSearchParams();
  const body: Record<string, unknown> = {};
  for (const [name, value] of Object.entries(parsed.values)) {
    const place = spec.in[name];
    if (place === "path") {
      path = path.replace(`{${name}}`, encodeURIComponent(String(value)));
    } else if (place === "query") {
      for (const item of Array.isArray(value) ? value : [value]) {
        if (item !== null && item !== undefined) query.append(name, String(item));
      }
    } else body[name] = value;
  }
  const search = query.toString();
  if (search) path = `${path}?${search}`;
  const response = await http.open({
    method: spec.method,
    path,
    ...(spec.input === "json" ? { json: body } : {}),
    ...(spec.input === "bytes" ? { body: [parsed.bytes ?? Buffer.alloc(0)] } : {}),
  });
  if (spec.output === "events") return streamEvents(io, response);
  const bytes = await readBody(response, Number.MAX_SAFE_INTEGER);
  if (spec.output === "bytes") {
    if (parsed.output) writeFileSync(parsed.output, bytes);
    else io.stdout(bytes);
    return 0;
  }
  const value: unknown = bytes.length > 0 ? JSON.parse(bytes.toString("utf8")) : null;
  if (parsed.quiet) {
    for (const id of ids(value)) io.stdout(`${id}\n`);
  } else if (parsed.json || !io.stdoutIsTty) io.stdout(`${JSON.stringify(value)}\n`);
  else io.stdout(`${JSON.stringify(value, null, 2)}\n`);
  if (parsed.open) {
    const url = value && typeof value === "object" ? (value as { url?: unknown }).url : undefined;
    if (typeof url !== "string") {
      throw new Exit(
        1,
        "kumobad returned no browser URL; start it with KUMOBAD_PREVIEW_LISTEN or KUMOBAD_PREVIEW_URL set"
      );
    }
    io.open(url);
  }
  return 0;
}

export async function main(argv: readonly string[], io: Io): Promise<number> {
  const tokens = [...argv];
  const first = tokens[0];
  try {
    if (first === "version" || first === "--version" || first === "-V") {
      io.stdout(`kumoba ${VERSION}\n`);
      try {
        const { url, token } = resolveOptions({ env: io.env });
        io.stdout(`kumobad ${(await fetchCatalog(new Http(url, token), url)).version}\n`);
      } catch {}
      return 0;
    }
    const wantsHelp =
      first === undefined || first === "help" || first === "--help" || first === "-h";
    let url = "";
    let catalog: Catalog;
    let http: Http;
    try {
      const resolved = resolveOptions({ env: io.env });
      url = resolved.url;
      http = new Http(resolved.url, resolved.token);
      catalog = await fetchCatalog(http, url);
    } catch (error) {
      if (wantsHelp && tokens.length <= 1) {
        overview(io, undefined, url, exitFor(error).message);
        return 0;
      }
      throw error;
    }
    if (
      first === undefined ||
      first === "--help" ||
      first === "-h" ||
      (first === "help" && !tokens[1])
    ) {
      overview(io, catalog, url);
      return 0;
    }
    if (first === "help") {
      const spec = catalog.commands.find((command) => command.name === tokens[1]);
      if (!spec) throw new UsageError(`unknown command ${String(tokens[1])}`);
      commandHelp(io, spec);
      return 0;
    }
    if (first === "commands") {
      if (tokens.includes("--json")) io.stdout(`${JSON.stringify(catalog, null, 2)}\n`);
      else
        for (const command of catalog.commands) {
          io.stdout(`${command.name.padEnd(16)} ${command.summary}\n`);
        }
      return 0;
    }
    const spec = catalog.commands.find((command) => command.name === first);
    if (!spec) throw new UsageError(`unknown command ${first}; \`kumoba help\` lists them`);
    return await invoke(io, http, spec, tokens.slice(1));
  } catch (error) {
    const exit = exitFor(error);
    io.stderr(`kumoba: ${exit.message}\n`);
    return exit.code;
  }
}

function isEntryPoint(): boolean {
  const script = process.argv[1];
  if (!script) return false;
  try {
    return realpathSync(script) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (isEntryPoint()) {
  const io: Io = {
    env: process.env,
    stdout: (chunk) => process.stdout.write(chunk),
    stderr: (chunk) => process.stderr.write(chunk),
    stdin: () => readFileSync(0),
    stdinIsTty: Boolean(process.stdin.isTTY),
    stdoutIsTty: Boolean(process.stdout.isTTY),
    open: (url) => {
      const opener = process.platform === "darwin" ? "open" : "xdg-open";
      spawnSync(opener, [url], { stdio: "ignore" });
    },
  };
  main(process.argv.slice(2), io).then(
    (code) => {
      process.exitCode = code;
    },
    (error: unknown) => {
      process.stderr.write(`kumoba: ${String(error)}\n`);
      process.exitCode = 1;
    }
  );
}
