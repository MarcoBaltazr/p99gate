#!/usr/bin/env python3
r"""Record the README demo as an asciicast (v2) file.

Runs the real `p99gate` binary in a pseudo-terminal and records its output
with the actual timing. Only the typing of the commands is simulated.

    cargo build --release
    python3 scripts/record-demo.py docs/demo.cast
    agg --font-family "DejaVu Sans Mono" --font-size 18 --theme monokai \
        --fps-cap 15 --last-frame-duration 6 docs/demo.cast docs/demo.gif

Requires a Unix-like system (uses the `pty` module).
"""

import codecs
import fcntl
import json
import os
import pty
import select
import struct
import sys
import termios
import time

COLS, ROWS = 92, 34
BINARY = os.path.join(os.path.dirname(__file__), "..", "target", "release", "p99gate")
PROMPT = "\x1b[1;32m$\x1b[0m "
TYPING_DELAY = 0.045

COMMANDS = [
    [
        "p99gate", "demo", "--rps", "200", "--duration", "8s",
        "--fail-if", "p95>100ms", "--fail-if", "p99>50ms",
    ],
]


class Cast:
    def __init__(self, path):
        self.file = open(path, "w", encoding="utf-8")
        self.start = time.monotonic()
        header = {
            "version": 2,
            "width": COLS,
            "height": ROWS,
            "timestamp": int(time.time()),
            "env": {"TERM": "xterm-256color", "SHELL": "/bin/bash"},
        }
        self.file.write(json.dumps(header) + "\n")

    def output(self, text):
        elapsed = time.monotonic() - self.start
        self.file.write(json.dumps([round(elapsed, 6), "o", text]) + "\n")

    def pause(self, seconds):
        time.sleep(seconds)

    def type(self, text):
        for char in text:
            self.output(char)
            time.sleep(TYPING_DELAY)


def shell_quote(arg):
    return f'"{arg}"' if any(c in arg for c in "<>|&;$ ") else arg


def run(cast, argv):
    """Runs argv in a pty of COLS x ROWS, recording output. Returns the exit code."""
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execv(BINARY, [BINARY] + argv[1:])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    decoder = codecs.getincrementaldecoder("utf-8")()
    while True:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            continue
        try:
            data = os.read(fd, 65536)
        except OSError:  # EIO: the child closed the terminal
            break
        if not data:
            break
        cast.output(decoder.decode(data))
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} OUTPUT.cast")
    cast = Cast(sys.argv[1])
    cast.pause(0.5)
    for argv in COMMANDS:
        cast.output(PROMPT)
        cast.pause(0.6)
        cast.type(" ".join(shell_quote(a) for a in argv))
        cast.pause(0.4)
        cast.output("\r\n")
        code = run(cast, argv)
        cast.output(PROMPT)
        cast.pause(0.8)
        cast.type("echo $?")
        cast.pause(0.3)
        cast.output(f"\r\n{code}\r\n")
        cast.output(PROMPT)
        cast.pause(3.5)
    # A final event so players hold the last frame.
    cast.output("")


if __name__ == "__main__":
    main()
