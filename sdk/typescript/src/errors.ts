export type KumobaErrorCode =
  | "config_invalid"
  | "unauthorized"
  | "not_found"
  | "invalid_request"
  | "capacity"
  | "guest_error"
  | "protocol"
  | "response_too_large"
  | "command_failed"
  | "daemon_error"
  | "unreachable"
  | string;

/**
 * Every failure the SDK raises. `status` mirrors the daemon's HTTP status when
 * the error came from kumobad, and `code` is a stable machine-readable tag.
 */
export class KumobaError extends Error {
  readonly status: number;
  readonly code: KumobaErrorCode;
  readonly retryable: boolean;

  constructor(
    message: string,
    options: { status?: number; code?: KumobaErrorCode; retryable?: boolean; cause?: unknown } = {}
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "KumobaError";
    this.status = options.status ?? 0;
    this.code = options.code ?? "daemon_error";
    this.retryable = options.retryable ?? false;
  }

  /** True when the daemon reported that the sandbox does not exist. */
  get notFound(): boolean {
    return this.status === 404;
  }
}
