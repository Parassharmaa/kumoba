export function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

/**
 * Turn what the caller passed to `run` into the absolute argv the daemon requires.
 * A string runs through `/bin/sh -c`; an argv whose program is not an absolute
 * path is resolved by the guest shell's PATH lookup.
 */
export function toArgv(command: string | readonly string[]): string[] {
  if (typeof command === "string") return ["/bin/sh", "-c", command];
  const [program, ...args] = command;
  if (program === undefined) throw new Error("run() needs a command");
  if (program.startsWith("/")) return [program, ...args];
  return ["/bin/sh", "-c", 'exec "$0" "$@"', program, ...args];
}
