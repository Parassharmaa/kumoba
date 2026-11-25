import { randomUUID } from "node:crypto";
import { z } from "zod";
import { KumobaError } from "./errors.js";
import type { Files } from "./files.js";
import { withDeadline } from "./http.js";
import { shellQuote } from "./shell.js";
import type {
  AccessibilityNode,
  CallOptions,
  DisplayInfo,
  Exposed,
  ExposeOptions,
  MouseButton,
  RunOptions,
  RunResult,
  ScreenshotOptions,
} from "./types.js";

const HELPER = "/usr/local/bin/kumoba-desktop";
const SCRATCH = "/tmp/kumoba-desktop";
/** Port of the noVNC websocket bridge the helper starts. */
export const DESKTOP_VNC_PORT = 6080;
/** Port of kumoba's own live view, which images built for 0.5 or later include. */
export const DESKTOP_STREAM_PORT = 6090;

const runningSchema = z.object({ running: z.boolean() });
const startSchema = z.record(z.string(), z.boolean());
const displaySchema = z.object({
  displays: z.array(z.object({ isActive: z.boolean(), width: z.number(), height: z.number() })),
});
const positionSchema = z.object({ x: z.number(), y: z.number() });
const nodeSchema: z.ZodType<AccessibilityNode> = z.lazy(() =>
  z.object({
    role: z.string().optional(),
    name: z.string().optional(),
    states: z.array(z.string()).optional(),
    bounds: z
      .object({
        x: z.number().optional(),
        y: z.number().optional(),
        width: z.number().optional(),
        height: z.number().optional(),
      })
      .optional(),
    children: z.array(nodeSchema).optional(),
  })
);
const treeSchema = z.object({ root: nodeSchema.nullable(), truncated: z.boolean() });

export type DesktopProcess = "xvfb" | "xfce4" | "x11vnc" | "novnc" | "view";

export type LiveView = Exposed & {
  /** The viewer the link opens. */
  viewer: "stream" | "vnc";
};

export type LiveViewOptions = ExposeOptions & {
  /** `stream` is kumoba's own viewer and the default; `vnc` is the noVNC bridge. */
  viewer?: "stream" | "vnc";
};

type Sh = (
  command: string,
  options?: Pick<RunOptions, "timeoutMs" | "signal">
) => Promise<RunResult>;

type Signal = AbortSignal | undefined;

function unexpectedOutput(args: readonly (string | number)[], cause: unknown): KumobaError {
  return new KumobaError(`desktop ${String(args[0])} printed unexpected output`, {
    status: 502,
    code: "protocol",
    cause,
  });
}

/**
 * Drive the Xvfb/xfce desktop in images built from `images/default`.
 * Every call shells out to `/usr/local/bin/kumoba-desktop` inside the guest.
 */
export class Desktop {
  constructor(
    private readonly sh: Sh,
    private readonly files: Files,
    private readonly expose: (port: number, options?: ExposeOptions) => Promise<Exposed>,
    private readonly hosted: (options: LiveViewOptions) => Promise<LiveView>
  ) {}

  private async helper(
    args: readonly (string | number)[],
    signal: Signal,
    timeoutMs = 30_000
  ): Promise<string> {
    const command = [HELPER, ...args.map((arg) => shellQuote(String(arg)))].join(" ");
    const result = await this.sh(command, { timeoutMs, ...(signal ? { signal } : {}) });
    if (!result.ok) {
      throw new KumobaError(
        result.stderr.trim() ||
          result.stdout.trim() ||
          `desktop ${String(args[0])} failed with exit ${String(result.exitCode)}`,
        { status: 502, code: "command_failed" }
      );
    }
    return result.stdout;
  }

  private async json<T>(
    schema: z.ZodType<T>,
    args: readonly (string | number)[],
    signal: Signal,
    timeoutMs?: number
  ): Promise<T> {
    const output = await this.helper(args, signal, timeoutMs);
    const line = output.trim().split("\n").at(-1) ?? "";
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch (cause) {
      throw unexpectedOutput(args, cause);
    }
    const parsed = schema.safeParse(value);
    if (!parsed.success) throw unexpectedOutput(args, parsed.error);
    return parsed.data;
  }

  private async command(args: readonly (string | number)[], options: CallOptions): Promise<void> {
    await withDeadline(options, (signal) => this.helper(args, signal));
  }

  /** Start Xvfb, the window manager and the live views if they are not running. */
  async start(options: CallOptions = {}): Promise<Record<string, boolean>> {
    return withDeadline(options, (signal) => this.json(startSchema, ["start"], signal, 60_000));
  }

  async status(process: DesktopProcess, options: CallOptions = {}): Promise<boolean> {
    return withDeadline(options, (signal) => this.running(process, signal));
  }

  private async running(process: DesktopProcess, signal: Signal): Promise<boolean> {
    return (await this.json(runningSchema, ["status", process], signal)).running;
  }

  async restart(process: DesktopProcess, options: CallOptions = {}): Promise<boolean> {
    const result = await withDeadline(options, (signal) =>
      this.json(runningSchema, ["restart", process], signal, 60_000)
    );
    return result.running;
  }

  async display(options: CallOptions = {}): Promise<DisplayInfo> {
    const info = await withDeadline(options, (signal) =>
      this.json(displaySchema, ["display"], signal)
    );
    const active = info.displays.find((display) => display.isActive) ?? info.displays[0];
    if (!active)
      throw new KumobaError("no display reported", { status: 502, code: "command_failed" });
    return { width: active.width, height: active.height };
  }

  /** Capture the screen. Returns encoded image bytes. */
  async screenshot(options: ScreenshotOptions = {}): Promise<Buffer> {
    const format = options.format ?? "png";
    const compressed = format === "jpeg" || (options.scale !== undefined && options.scale !== 1);
    const path = `${SCRATCH}/${randomUUID()}.${format === "jpeg" ? "jpg" : "png"}`;
    return withDeadline(options, async (signal) => {
      await this.helper(
        [
          "screenshot",
          path,
          compressed ? "compressed" : "full",
          format,
          options.quality ?? 75,
          options.scale ?? 1,
          options.cursor ? 1 : 0,
        ],
        signal
      );
      try {
        return await this.files.read(path, { signal });
      } finally {
        await this.files.remove(path);
      }
    });
  }

  async click(
    x: number,
    y: number,
    options: CallOptions & { button?: MouseButton; double?: boolean } = {}
  ): Promise<void> {
    await this.command(["click", x, y, options.button ?? "left", options.double ? 1 : 0], options);
  }

  async move(x: number, y: number, options: CallOptions = {}): Promise<void> {
    await this.command(["move", x, y], options);
  }

  async drag(
    from: { x: number; y: number },
    to: { x: number; y: number },
    button: MouseButton = "left",
    options: CallOptions = {}
  ): Promise<void> {
    await this.command(["drag", from.x, from.y, to.x, to.y, button], options);
  }

  async scroll(
    x: number,
    y: number,
    direction: "up" | "down",
    amount = 1,
    options: CallOptions = {}
  ): Promise<void> {
    await this.command(["scroll", x, y, direction, amount], options);
  }

  async position(options: CallOptions = {}): Promise<{ x: number; y: number }> {
    return withDeadline(options, (signal) => this.json(positionSchema, ["position"], signal));
  }

  /** Type text with xdotool. Multi-line and non-ASCII text is fine. */
  async type(text: string, options: CallOptions = {}): Promise<void> {
    const path = `${SCRATCH}/${randomUUID()}.txt`;
    await withDeadline(options, async (signal) => {
      await this.files.mkdir(SCRATCH, { signal });
      await this.files.write(path, text, { signal });
      const result = await this.sh(
        `${HELPER} type < ${shellQuote(path)}; status=$?; rm -f ${shellQuote(path)}; exit $status`,
        { timeoutMs: 120_000, ...(signal ? { signal } : {}) }
      );
      if (!result.ok) {
        throw new KumobaError(result.stderr.trim() || "typing failed", {
          status: 502,
          code: "command_failed",
        });
      }
    });
  }

  /** Press one key with optional modifiers, for example `press("a", ["ctrl"])`. */
  async press(key: string, modifiers: string[] = [], options: CallOptions = {}): Promise<void> {
    await this.command(["press", key, ...modifiers], options);
  }

  /** Press a combination written as `"ctrl+shift+t"`. */
  async hotkey(keys: string, options: CallOptions = {}): Promise<void> {
    await this.command(["hotkey", keys], options);
  }

  /** Dump the AT-SPI accessibility tree. */
  async accessibilityTree(
    options: CallOptions & { maxDepth?: number } = {}
  ): Promise<{ root: AccessibilityNode | null; truncated: boolean }> {
    const path = `${SCRATCH}/${randomUUID()}.json`;
    return withDeadline(options, async (signal) => {
      await this.helper(["a11y-file", options.maxDepth ?? 8, path], signal, 60_000);
      try {
        return treeSchema.parse(JSON.parse(await this.files.readText(path, { signal })));
      } finally {
        await this.files.remove(path);
      }
    });
  }

  /**
   * A time-limited link to the live view, and which viewer it opens. `stream` is kumoba's
   * own viewer and the link is its page. `vnc` is the noVNC bridge, where `vnc.html` under
   * the link is the page; an image without the stream viewer gets it, and so does
   * `viewer: "vnc"`. On a Linux sandbox the desktop must be started first. A macOS sandbox
   * has no desktop helper inside it: kumobad reads its screen on the host, and the link is
   * always the `stream` viewer.
   */
  async liveView(options: LiveViewOptions = {}): Promise<LiveView> {
    return withDeadline(options, async (signal) => {
      const viewer = options.viewer;
      const ttl = options.ttlSeconds === undefined ? {} : { ttlSeconds: options.ttlSeconds };
      const running = await this.running("view", signal).catch((error: unknown) => {
        if (signal?.aborted) throw error;
        return null;
      });
      if (running === null && !(await this.sh(`test -x ${HELPER}`, signal ? { signal } : {})).ok) {
        return this.hosted({ ...ttl, ...(viewer ? { viewer } : {}), signal });
      }
      const stream = viewer !== "vnc" && running === true;
      const link = await this.expose(stream ? DESKTOP_STREAM_PORT : DESKTOP_VNC_PORT, {
        ...ttl,
        signal,
      });
      return { ...link, viewer: stream ? "stream" : "vnc" };
    });
  }
}
