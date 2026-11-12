export type NetworkMode = "open" | "allowlist" | "blocked";

/** The network policy as the daemon stores it. */
export type Network = { mode: NetworkMode; hosts: string[] };

/**
 * How a new sandbox may reach the network.
 * - `"open"` lets it reach any public host (private and link-local ranges are always blocked).
 * - `"blocked"` gives it no network at all.
 * - `{ allow: [...] }` permits only the listed hosts.
 */
export type NetworkOption = "open" | "blocked" | { allow: string[] };

export type SandboxState = "started" | "stopped" | "destroyed";

export type SandboxInfo = {
  id: string;
  image: string;
  labels: Record<string, string>;
  cpu: number;
  memoryMib: number;
  autoStopMinutes: number;
  autoDeleteMinutes: number | null;
  network: Network;
  state: SandboxState;
  createdAt: Date;
  lastActivityAt: Date;
};

export type CreateOptions = {
  /** Image name as registered with kumobad (`KUMOBAD_IMAGES`). Defaults to `"default"`. */
  image?: string;
  /** Virtual CPUs. Defaults to 2. */
  cpu?: number;
  /** Guest memory in MiB. Defaults to 4096. */
  memoryMib?: number;
  /** Free-form labels; `list({ labels })` filters on them. */
  labels?: Record<string, string>;
  /** Minutes of inactivity before kumobad stops the VM. 0 disables auto-stop. Defaults to 15. */
  autoStopMinutes?: number;
  /** Minutes a stopped sandbox is kept before kumobad deletes it. `null` keeps it forever. */
  autoDeleteMinutes?: number | null;
  network?: NetworkOption;
  /** Guest user that `run` and `files` act as unless a call overrides it. Defaults to root. */
  user?: string;
  /** Boot the VM right away. Defaults to true. */
  start?: boolean;
};

export type ListOptions = {
  labels?: Record<string, string>;
  limit?: number;
};

export type UpdateOptions = {
  labels?: Record<string, string>;
  autoStopMinutes?: number;
};

export type RunOptions = {
  /** Working directory inside the guest. */
  cwd?: string;
  env?: Record<string, string>;
  /** Kill the command after this long. Defaults to 10 minutes. */
  timeoutMs?: number;
  /** Guest user to run as. Overrides the sandbox default. */
  user?: string;
  /** Called with decoded text as stdout arrives. */
  onStdout?: (text: string) => void;
  /** Called with decoded text as stderr arrives. */
  onStderr?: (text: string) => void;
  /** Abort the request; the daemon kills the guest process group. */
  signal?: AbortSignal;
  /** Bytes of stdout and of stderr to keep in the result. Defaults to 1 MiB each. */
  maxOutputBytes?: number;
};

export type RunResult = {
  /** Process exit code, or `null` when it timed out or was killed by the daemon. */
  exitCode: number | null;
  stdout: string;
  stderr: string;
  timedOut: boolean;
  /** Signal number when the process died from a signal. */
  signal?: number;
  /** True when `exitCode === 0`. */
  ok: boolean;
  /** True when stdout or stderr exceeded `maxOutputBytes` and was cut. */
  truncated: boolean;
};

export type FileEntry = {
  name: string;
  kind: "file" | "directory" | "other";
  size: number;
};

export type FileOptions = {
  /** Guest user to act as. Overrides the sandbox default. */
  user?: string;
};

export type ReadOptions = FileOptions & {
  /** Refuse files larger than this. Defaults to 64 MiB. */
  maxBytes?: number;
};

export type Secret = {
  name: string;
  value: string;
  /** Hosts whose HTTPS requests receive the real value. */
  hosts: string[];
  /** Environment variables that carry the placeholder inside the guest. */
  envVars: string[];
};

export type ExposeOptions = {
  /** Seconds the link stays valid (1 to 3600). Defaults to 600. */
  ttlSeconds?: number;
};

export type Exposed = {
  /** Path on the daemon, `/v1/preview/<token>/`. */
  path: string;
  /** Full URL when the client knows a reachable base URL, otherwise `null`. */
  url: string | null;
  expiresAt: Date;
};

export type ScreenshotOptions = {
  format?: "png" | "jpeg";
  /** JPEG quality, 1 to 100. Defaults to 75. */
  quality?: number;
  /** Resize factor, for example 0.5. Defaults to 1. */
  scale?: number;
  /** Draw the mouse cursor. Defaults to false. */
  cursor?: boolean;
};

export type MouseButton = "left" | "middle" | "right";

export type AccessibilityNode = {
  role?: string | undefined;
  name?: string | undefined;
  states?: string[] | undefined;
  bounds?:
    | { x?: number | undefined; y?: number | undefined; width?: number | undefined; height?: number | undefined }
    | undefined;
  children?: AccessibilityNode[] | undefined;
};

export type DisplayInfo = { width: number; height: number };
