import { readFileSync } from "node:fs";

export type JsonSchema = {
  type?: string | string[];
  enum?: unknown[];
  items?: JsonSchema;
  properties?: Record<string, JsonSchema>;
  additionalProperties?: unknown;
  required?: string[];
  description?: string;
  default?: unknown;
};

export type CommandSpec = {
  name: string;
  summary: string;
  method: string;
  path: string;
  positional: string[];
  rest: string | null;
  input: "none" | "json" | "bytes";
  output: "json" | "events" | "bytes";
  in: Record<string, "path" | "query" | "body" | "bytes">;
  inputSchema: JsonSchema;
};

export type Catalog = { version: string; commands: CommandSpec[] };

export type Parsed = {
  values: Record<string, unknown>;
  bytes?: Buffer;
  help: boolean;
  quiet: boolean;
  json: boolean;
  open: boolean;
  output?: string;
};

export class UsageError extends Error {}

export const camel = (name: string): string =>
  name.replace(/-([a-z0-9])/g, (_, letter: string) => letter.toUpperCase());

export const kebab = (name: string): string =>
  name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);

export function types(schema: JsonSchema): string[] {
  if (typeof schema.type === "string") return [schema.type];
  if (Array.isArray(schema.type)) return schema.type;
  return schema.enum ? ["string"] : [];
}

const primary = (schema: JsonSchema): string =>
  types(schema).find((kind) => kind !== "null") ?? "string";

function property(root: JsonSchema, path: string[]): JsonSchema | undefined {
  let schema: JsonSchema | undefined = root;
  for (const name of path) schema = schema?.properties?.[name];
  return schema;
}

export function readSource(value: string, stdin: () => Buffer): Buffer {
  if (value === "@-") return stdin();
  if (value.startsWith("@@")) return Buffer.from(value.slice(1));
  if (value.startsWith("@")) {
    try {
      return readFileSync(value.slice(1));
    } catch (error) {
      throw new UsageError(`cannot read ${value.slice(1)}: ${(error as Error).message}`);
    }
  }
  return Buffer.from(value);
}

function scalar(schema: JsonSchema, raw: string, name: string): unknown {
  if (raw === "null" && types(schema).includes("null")) return null;
  const invalid = (kind: string) => new UsageError(`${name} must be ${kind}, got ${raw}`);
  switch (primary(schema)) {
    case "integer":
      if (!/^-?\d+$/.test(raw)) throw invalid("an integer");
      return Number(raw);
    case "number":
      if (raw.trim() === "" || Number.isNaN(Number(raw))) throw invalid("a number");
      return Number(raw);
    case "boolean":
      if (["true", "1", "yes"].includes(raw)) return true;
      if (["false", "0", "no"].includes(raw)) return false;
      throw invalid("true or false");
    default:
      if (schema.enum && !schema.enum.includes(raw)) {
        throw new UsageError(`${name} must be one of ${schema.enum.map(String).join(", ")}`);
      }
      return raw;
  }
}

function parseJson(raw: string, name: string): unknown {
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new UsageError(`${name}: ${(error as Error).message}`);
  }
}

function coerce(
  schema: JsonSchema,
  raw: string,
  name: string,
  current: unknown,
  stdin: () => Buffer
): unknown {
  const kind = primary(schema);
  if (kind === "object" && !raw.startsWith("@") && !raw.trimStart().startsWith("{")) {
    const split = raw.indexOf("=");
    if (split < 0) throw new UsageError(`${name} takes JSON or key=value pairs, got ${raw}`);
    const base = current && typeof current === "object" ? (current as Record<string, unknown>) : {};
    return {
      ...base,
      [raw.slice(0, split)]: readSource(raw.slice(split + 1), stdin).toString("utf8"),
    };
  }
  const text = readSource(raw, stdin).toString("utf8");
  if (kind === "array") {
    if (text.trimStart().startsWith("[")) return parseJson(text, name);
    const existing = Array.isArray(current) ? current : [];
    const parts = text
      .split(/[,\n]/)
      .map((part) => part.trim())
      .filter(Boolean);
    return [...existing, ...parts.map((part) => scalar(schema.items ?? {}, part, name))];
  }
  if (kind === "object") return parseJson(text, name);
  return scalar(schema, text, name);
}

function insert(values: Record<string, unknown>, path: string[], value: unknown): void {
  let target = values;
  for (const name of path.slice(0, -1)) {
    const next = target[name];
    if (!next || typeof next !== "object" || Array.isArray(next)) target[name] = {};
    target = target[name] as Record<string, unknown>;
  }
  target[path[path.length - 1] as string] = value;
}

function get(values: Record<string, unknown>, path: string[]): unknown {
  let target: unknown = values;
  for (const name of path) {
    if (!target || typeof target !== "object") return undefined;
    target = (target as Record<string, unknown>)[name];
  }
  return target;
}

export function bytesField(spec: CommandSpec): string | undefined {
  return Object.entries(spec.in).find(([, place]) => place === "bytes")?.[0];
}

export function parseArgs(
  spec: CommandSpec,
  tokens: readonly string[],
  stdin: () => Buffer = () => readFileSync(0)
): Parsed {
  const parsed: Parsed = { values: {}, help: false, quiet: false, json: false, open: false };
  const split = tokens.indexOf("--");
  const before = split < 0 ? tokens : tokens.slice(0, split);
  const rest = split < 0 ? undefined : tokens.slice(split + 1);
  const bytes = bytesField(spec);
  const pathOf = (key: string) => {
    const path = key.split(".").map(camel);
    return property(spec.inputSchema, path) ? path : undefined;
  };
  const set = (path: string[], raw: string) => {
    const name = path.join(".");
    if (name === bytes) {
      parsed.bytes = readSource(raw, stdin);
      return;
    }
    const schema = property(spec.inputSchema, path) as JsonSchema;
    insert(parsed.values, path, coerce(schema, raw, kebab(name), get(parsed.values, path), stdin));
  };
  let positional = 0;
  for (let index = 0; index < before.length; index += 1) {
    const token = before[index] as string;
    const next = () => {
      index += 1;
      const value = before[index];
      if (value === undefined) throw new UsageError(`${token} needs a value`);
      return value;
    };
    if (token === "--help" || token === "-h") parsed.help = true;
    else if (token === "--quiet" || token === "-q") parsed.quiet = true;
    else if (token === "--json") parsed.json = true;
    else if (token === "--open") parsed.open = true;
    else if (token === "--output" || token === "-o") parsed.output = next();
    else if (token === "--args") {
      const object = parseJson(readSource(next(), stdin).toString("utf8"), "--args");
      if (!object || typeof object !== "object" || Array.isArray(object)) {
        throw new UsageError("--args must be a JSON object");
      }
      Object.assign(parsed.values, object);
    } else if (token.startsWith("--")) {
      const flag = token.slice(2);
      const eq = flag.indexOf("=");
      const key = eq < 0 ? flag : flag.slice(0, eq);
      const path = pathOf(key);
      if (!path) throw new UsageError(`unknown option --${key}`);
      const schema = property(spec.inputSchema, path) as JsonSchema;
      let raw: string;
      if (eq >= 0) raw = flag.slice(eq + 1);
      else if (
        primary(schema) === "boolean" &&
        (before[index + 1] === undefined || (before[index + 1] as string).startsWith("--"))
      ) {
        raw = "true";
      } else raw = next();
      set(path, raw);
    } else {
      const eq = token.indexOf("=");
      const path = eq > 0 ? pathOf(token.slice(0, eq)) : undefined;
      if (path) set(path, token.slice(eq + 1));
      else {
        const name = spec.positional[positional];
        if (!name) throw new UsageError(`unexpected argument ${token}`);
        positional += 1;
        set([name], token);
      }
    }
  }
  if (rest) {
    if (!spec.rest) throw new UsageError("this command takes no arguments after --");
    parsed.values[spec.rest] = [...rest];
  }
  return parsed;
}

export function missing(spec: CommandSpec, parsed: Parsed): string[] {
  const bytes = bytesField(spec);
  return (spec.inputSchema.required ?? []).filter((name) =>
    name === bytes ? parsed.bytes === undefined : !(name in parsed.values)
  );
}
