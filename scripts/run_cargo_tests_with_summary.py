"""Run a cargo test command and expose a bounded, redacted failure summary."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from collections import deque
from pathlib import Path


ANSI_ESCAPE = re.compile(
    r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))"
)
FAILED_TEST = re.compile(r"^\s*test\s+([^\s]+)\s+\.\.\.\s+FAILED(?:\s|$)")
FAILED_TEST_DETAIL = re.compile(r"^\s*----\s+(.+?)\s+stdout ----\s*$")
SECRET_ASSIGNMENT = re.compile(
    r"(?i)\b(authorization|access[_-]?token|refresh[_-]?token|token|password|"
    r"passwd|secret|credential|api[_-]?key|capability)\b(\s*[:=]\s*|\s+)([^\s,;]+)"
)
BEARER_TOKEN = re.compile(r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]+=*")
TOKEN_PREFIX = re.compile(
    r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
    r"sk-[A-Za-z0-9_-]{20,})\b"
)
MAX_SUMMARY_LINES = 80
MAX_LINE_LENGTH = 500
MAX_FAILED_TESTS = 20
MAX_TEST_NAME_LENGTH = 300


def redact(line: str) -> str:
    line = ANSI_ESCAPE.sub("", line)
    line = BEARER_TOKEN.sub("Bearer [REDACTED]", line)
    line = SECRET_ASSIGNMENT.sub(r"\1\2[REDACTED]", line)
    line = TOKEN_PREFIX.sub("[REDACTED_TOKEN]", line)
    line = "".join(char if char in "\t" or ord(char) >= 32 else " " for char in line)
    line = line.replace("```", "'''").rstrip()
    if len(line) > MAX_LINE_LENGTH:
        line = line[: MAX_LINE_LENGTH - 1] + "…"
    return line


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", required=True, help="Human-readable test step name")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="Command after --")
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("provide a command after --")

    tail: deque[str] = deque(maxlen=MAX_SUMMARY_LINES)
    failed_tests: list[str] = []
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,
    )
    assert process.stdout is not None
    for raw_line in process.stdout:
        match = FAILED_TEST.match(raw_line) or FAILED_TEST_DETAIL.match(raw_line)
        if (
            match
            and match.group(1) not in failed_tests
            and len(failed_tests) < MAX_FAILED_TESTS
        ):
            failed_tests.append(match.group(1)[:MAX_TEST_NAME_LENGTH])
        tail.append(raw_line.rstrip("\r\n"))
        print(redact(raw_line.rstrip("\r\n")), flush=True)
    return_code = process.wait()

    if return_code == 0:
        print(f"{args.title}: passed (exit code 0)")
        return 0

    summary = [f"## {args.title} failed", "", f"Exit code: `{return_code}`", ""]
    if failed_tests:
        summary.extend(["Failed test names:", ""])
        summary.extend(f"- `{redact(name)}`" for name in failed_tests)
    else:
        summary.extend(
            [
                "No individual failed test name was parsed; this may be a compile or harness failure.",
                "",
            ]
        )
    summary.extend(
        ["", "Bounded, redacted tail of combined Cargo output:", "", "```text"]
    )
    summary.extend(redact(line) for line in tail)
    summary.extend(["```", ""])
    rendered = "\n".join(summary)

    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with Path(summary_path).open("a", encoding="utf-8", newline="\n") as output:
            output.write(rendered)
    else:
        print(rendered, file=sys.stderr)
    return return_code


if __name__ == "__main__":
    raise SystemExit(main())
