"""Drives `kumoba shell` through a real pseudo-terminal, as a person at a keyboard would.

Called by scripts/cli-smoke.sh for both CLIs against a running kumobad:
    python3 scripts/shell-pty-test.py <sandbox-id> <cli command...>
Checks that the terminal size reaches the sandbox, that a resize is passed on, that
Ctrl-C interrupts a command without ending the shell, and that `exit 5` becomes the
CLI's exit status. It keeps reading the terminal while the CLI exits, as a terminal
emulator does: Node restores the terminal with TCSADRAIN, which waits for that.
"""

import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time


def set_size(fd, rows, cols):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def main():
    sandbox, cli = sys.argv[1], sys.argv[2:]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(cli[0], [*cli, "shell", sandbox])
    set_size(fd, 40, 120)
    os.kill(pid, signal.SIGWINCH)
    seen = b""

    def expect(needle, timeout=20):
        nonlocal seen
        deadline = time.time() + timeout
        while needle.encode() not in seen:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise SystemExit(f"FAIL waiting for {needle!r}; got {seen[-400:]!r}")
            ready, _, _ = select.select([fd], [], [], remaining)
            if ready:
                try:
                    chunk = os.read(fd, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                seen += chunk
        if needle.encode() not in seen:
            raise SystemExit(f"FAIL {needle!r} never arrived; got {seen[-400:]!r}")
        seen = seen[seen.index(needle.encode()) + len(needle):]

    def send(text):
        os.write(fd, text.encode())

    time.sleep(1.5)
    send("stty size; echo size-done\r")
    expect("40 120")
    expect("size-done")
    print("ok   pty size")
    set_size(fd, 50, 132)
    os.kill(pid, signal.SIGWINCH)
    time.sleep(0.5)
    send("stty size; echo resize-done\r")
    expect("50 132")
    expect("resize-done")
    print("ok   pty resize")
    send("sleep 30; echo slept\r")
    time.sleep(1)
    send("\x03")
    time.sleep(0.5)
    send("echo still-here\r")
    expect("still-here")
    if b"slept" in seen:
        raise SystemExit("FAIL Ctrl-C did not interrupt sleep")
    print("ok   pty ctrl-c")
    send("exit 5\r")
    deadline = time.time() + 20
    while True:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            break
        if time.time() > deadline:
            raise SystemExit("FAIL the CLI did not exit after the shell did")
        ready, _, _ = select.select([fd], [], [], 0.1)
        if ready:
            try:
                os.read(fd, 4096)
            except OSError:
                pass
    code = os.waitstatus_to_exitcode(status)
    if code != 5:
        raise SystemExit(f"FAIL exit status {code}, expected 5")
    print("ok   pty exit status")


main()
