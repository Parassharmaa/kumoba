import { randomUUID } from "node:crypto";
import { z } from "zod";
import { KumobaError } from "./errors.js";
import type { Files } from "./files.js";
import { shellQuote } from "./shell.js";
import type {
  AccessibilityNode,
  DisplayInfo,
  Exposed,
  ExposeOptions,
  MouseButton,
  RunResult,
  ScreenshotOptions,
} from "./types.js";

const HELPER = "/usr/local/bin/kumoba-desktop";
const SCRATCH = "/tmp/kumoba-desktop";
/** Port of the noVNC websocket bridge the helper starts. */
export const DESKTOP_VNC_PORT = 6080;

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

export type DesktopProcess = "xvfb" | "xfce4" | "x11vnc" | "novnc";

type Sh = (command: string, options?: { timeoutMs?: number }) => Promise<RunResult>;

/**
 * Drive the Xvfb/xfce desktop in images built from `images/default`.
 * Every call shells out to `/usr/local/bin/kumoba-desktop` inside the guest.
 */
export class Desktop {
  constructor(
    private readonly sh: Sh,
    private readonly files: Files,
    private readonly expose: (port: number, options?: ExposeOptions) => Promise<Exposed>
  ) {}

  private async helper(args: readonly (string | number)[], timeoutMs = 30_000): Promise<string> {
    const command = [HELPER, ...args.map((arg) => shellQuote(String(arg)))].join(" ");
    const result = await this.sh(command, { timeoutMs });
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
    timeoutMs?: number
  ): Promise<T> {
    const output = await this.helper(args, timeoutMs);
    const line = output.trim().split("\n").at(-1) ?? "";
    return schema.parse(JSON.parse(line));
  }

  /** Start Xvfb, the window manager, x11vnc and the noVNC bridge if they are not running. */
  async start(): Promise<Record<string, boolean>> {
    return this.json(startSchema, ["start"], 60_000);
  }

  async status(process: DesktopProcess): Promise<boolean> {
    return (await this.json(runningSchema, ["status", process])).running;
  }

  async restart(process: DesktopProcess): Promise<boolean> {
    return (await this.json(runningSchema, ["restart", process], 60_000)).running;
  }

  async display(): Promise<DisplayInfo> {
    const info = await this.json(displaySchema, ["display"]);
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
    await this.helper([
      "screenshot",
      path,
      compressed ? "compressed" : "full",
      format,
      options.quality ?? 75,
      options.scale ?? 1,
      options.cursor ? 1 : 0,
    ]);
    try {
      return await this.files.read(path);
    } finally {
      await this.files.remove(path);
    }
  }

  async click(
    x: number,
    y: number,
    options: { button?: MouseButton; double?: boolean } = {}
  ): Promise<void> {
    await this.helper(["click", x, y, options.button ?? "left", options.double ? 1 : 0]);
  }

  async move(x: number, y: number): Promise<void> {
    await this.helper(["move", x, y]);
  }

  async drag(
    from: { x: number; y: number },
    to: { x: number; y: number },
    button: MouseButton = "left"
  ): Promise<void> {
    await this.helper(["drag", from.x, from.y, to.x, to.y, button]);
  }

  async scroll(x: number, y: number, direction: "up" | "down", amount = 1): Promise<void> {
    await this.helper(["scroll", x, y, direction, amount]);
  }

  async position(): Promise<{ x: number; y: number }> {
    return this.json(positionSchema, ["position"]);
  }

  /** Type text with xdotool. Multi-line and non-ASCII text is fine. */
  async type(text: string): Promise<void> {
    const path = `${SCRATCH}/${randomUUID()}.txt`;
    await this.files.mkdir(SCRATCH);
    await this.files.write(path, text);
    const result = await this.sh(
      `${HELPER} type < ${shellQuote(path)}; status=$?; rm -f ${shellQuote(path)}; exit $status`,
      { timeoutMs: 120_000 }
    );
    if (!result.ok) {
      throw new KumobaError(result.stderr.trim() || "typing failed", {
        status: 502,
        code: "command_failed",
      });
    }
  }

  /** Press one key with optional modifiers, for example `press("a", ["ctrl"])`. */
  async press(key: string, modifiers: string[] = []): Promise<void> {
    await this.helper(["press", key, ...modifiers]);
  }

  /** Press a combination written as `"ctrl+shift+t"`. */
  async hotkey(keys: string): Promise<void> {
    await this.helper(["hotkey", keys]);
  }

  /** Dump the AT-SPI accessibility tree. */
  async accessibilityTree(
    options: { maxDepth?: number } = {}
  ): Promise<{ root: AccessibilityNode | null; truncated: boolean }> {
    const path = `${SCRATCH}/${randomUUID()}.json`;
    await this.helper(["a11y-file", options.maxDepth ?? 8, path], 60_000);
    try {
      return treeSchema.parse(JSON.parse(await this.files.readText(path)));
    } finally {
      await this.files.remove(path);
    }
  }

  /**
   * A time-limited link to the noVNC websocket bridge, usable from a browser
   * with a noVNC client. The desktop must be started first.
   */
  async liveView(options?: ExposeOptions): Promise<Exposed> {
    return this.expose(DESKTOP_VNC_PORT, options);
  }
}
