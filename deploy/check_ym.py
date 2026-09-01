#!/usr/bin/env python3
"""Regression checks for the operator CLI timeout and failure boundary."""

import os
from pathlib import Path
import re
import shutil
import subprocess
from typing import Optional


SCRIPT = Path(__file__).resolve().parent / "ym"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def find_bash() -> str:
    bash = None
    if os.name == "nt":
        candidate = (
            Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
            / "Git"
            / "bin"
            / "bash.exe"
        )
        if candidate.is_file():
            bash = str(candidate)
    if bash is None:
        bash = shutil.which("bash")
    if bash is None:
        raise AssertionError("bash is required to exercise the operator wrapper")
    return bash


def run_sender_prefix(
    source: str,
    timeout: str,
    tail: str,
    token_file: Optional[str] = None,
    ctl_url: Optional[str] = None,
    box: Optional[str] = None,
) -> subprocess.CompletedProcess[str]:
    """Run only ym's setup/functions with a fake transport; never reach SSH or the live box."""
    marker = '\ncmd="${1:-}"'
    require(marker in source, "ym dispatcher marker changed; update the isolated behavior harness")
    prefix = source.split(marker, 1)[0]
    env = os.environ.copy()
    env.update(
        {
            "YM_BOX": "test@127.0.0.1",
            "YM_TOKEN_FILE": "/tmp/yantrik-test-token",
            "YM_CTL_URL": "http://127.0.0.1:8077",
            "YM_CLI_TIMEOUT_SECS": timeout,
        }
    )
    if token_file is not None:
        env["YM_TOKEN_FILE"] = token_file
    if ctl_url is not None:
        env["YM_CTL_URL"] = ctl_url
    if box is not None:
        env["YM_BOX"] = box
    return subprocess.run(
        [find_bash(), "-s"],
        input=f"{prefix}\n{tail}\n",
        text=True,
        encoding="utf-8",
        capture_output=True,
        env=env,
        check=False,
    )


def main() -> int:
    source = SCRIPT.read_text(encoding="utf-8")
    default = re.search(
        r'YM_CLI_TIMEOUT_SECS="\$\{YM_CLI_TIMEOUT_SECS:-(\d+)\}"', source
    )
    require(default is not None, "ym must expose a bounded CLI timeout override")
    require(
        int(default.group(1)) >= 240,
        "the default must cover the measured 195s agentic-turn tail with margin",
    )
    require(
        "*[!0-9]*|0)" in source,
        "the timeout must reject shell syntax, non-numbers, and zero before interpolation",
    )
    require(
        source.index('case "$YM_CLI_TIMEOUT_SECS" in')
        < source.index("ym_send()"),
        "timeout validation must run before the SSH-backed sender can be called",
    )
    require(
        source.index('case "$YM_TOKEN_FILE" in') < source.index("ym_send()"),
        "token-file validation must run before remote-shell interpolation",
    )
    require(
        source.index('case "$CTL" in') < source.index("ym_send()"),
        "control-URL validation must run before remote-shell interpolation",
    )
    require(
        'case "$BOX" in' in source and source.index('case "$BOX" in') < source.index("ssh_box()"),
        "the SSH destination must reject option-shaped overrides before invocation",
    )
    require(
        "curl -sS --fail-with-body -m $YM_CLI_TIMEOUT_SECS" in source,
        "ym_send must use the validated timeout and return non-zero for HTTP errors",
    )
    require("curl -s -m 150" not in source, "the obsolete 150s ceiling returned")
    sender_start = source.index("ym_send()")
    sender = source[sender_start : source.index("\n}", sender_start)]
    require(
        "rc=$?" in sender and 'return "$rc"' in sender,
        "ym_send must preserve a curl or SSH failure instead of masking it with the trailing newline",
    )
    require(
        '${*:--n 40}' not in source and 'ym_logs_command "$@"' in source,
        "the logs command must parse documented flags instead of interpolating raw arguments",
    )
    require(
        "bash -o pipefail -c 'bash deploy/self_deploy.sh 2>&1 | tail -3'" in source,
        "the deploy preview pipeline must propagate a failed provenance-gated deploy",
    )
    pipefail_probe = subprocess.run(
        [find_bash(), "-o", "pipefail", "-c", "(exit 23) | :"],
        capture_output=True,
        check=False,
    )
    require(
        pipefail_probe.returncode == 23,
        f"the deploy pipeline shell must preserve an upstream failure (rc={pipefail_probe.returncode})",
    )
    require(
        'ssh_box "curl -sS --fail-with-body -m 6 $CTL/status"' in source
        and 'return "$status_rc"' in source
        and "ym_status" in source
        and "|| echo UNREACHABLE" not in source,
        "status must report HTTP failures and return non-zero when either health check fails",
    )

    for invalid in ("0", "12x", "300; exit 0", "3601", "123456"):
        result = run_sender_prefix(source, invalid, "exit 91")
        require(
            result.returncode == 2,
            f"timeout override {invalid!r} must be refused before the transport (rc={result.returncode})",
        )

    for invalid in ("relative/token", "/tmp/token;exit"):
        result = run_sender_prefix(source, "300", "exit 91", token_file=invalid)
        require(
            result.returncode == 2,
            f"token-file override {invalid!r} must be refused before the transport (rc={result.returncode})",
        )

    result = run_sender_prefix(
        source,
        "300",
        "exit 91",
        box="-oProxyCommand=unexpected",
    )
    require(
        result.returncode == 2,
        f"option-shaped SSH destination must be refused before invocation (rc={result.returncode})",
    )

    for invalid in (
        "http://example.com:8077",
        "http://127.0.0.1:8077;exit",
        "http://127.0.0.1:",
        "http://127.0.0.1:not-a-port",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:123456",
        "http://127.0.0.1::8077",
        "http://127.0.0.1:8077/path:9",
    ):
        result = run_sender_prefix(source, "300", "exit 91", ctl_url=invalid)
        require(
            result.returncode == 2,
            f"control-URL override {invalid!r} must be refused before the transport (rc={result.returncode})",
        )

    result = run_sender_prefix(
        source,
        "300",
        'ssh_box() { return 23; }\nym_send "payload"\nexit $?',
    )
    require(
        result.returncode == 23,
        f"ym_send must propagate the fake transport failure (rc={result.returncode})",
    )

    result = run_sender_prefix(
        source,
        "300",
        """ssh_box() { printf '%s' "$1"; return 0; }
ym_send "payload"
exit $?""",
    )
    require(
        result.returncode == 0,
        f"a valid timeout must reach the fake transport (rc={result.returncode})",
    )
    require(
        "curl -sS --fail-with-body -m 300" in result.stdout,
        "the validated timeout and HTTP-failure flag must reach the remote curl command",
    )

    for label, invocation in (
        ("shell syntax", "ym_logs_command '; touch /tmp/pwn'"),
        ("unknown flag", "ym_logs_command --since"),
        ("nonnumeric count", "ym_logs_command -n not-a-number"),
        ("oversized count", "ym_logs_command -n 10001"),
    ):
        result = run_sender_prefix(
            source,
            "300",
            f"{invocation} >/dev/null\nexit $?",
        )
        require(
            result.returncode == 2,
            f"logs {label} must be refused before SSH (rc={result.returncode})",
        )

    result = run_sender_prefix(
        source,
        "300",
        "ym_logs_command --follow --lines 80\nexit $?",
    )
    require(result.returncode == 0, "documented logs flags must be accepted")
    require(
        result.stdout == "journalctl -u yantrik-mind --no-pager -n 80 -f",
        f"logs flags must render one constant-shape command (got {result.stdout!r})",
    )

    result = run_sender_prefix(source, "300", "ym_logs_command\nexit $?")
    require(result.returncode == 0, "logs without flags must preserve the bounded default")
    require(
        result.stdout == "journalctl -u yantrik-mind --no-pager -n 40",
        f"default logs command must remain bounded at 40 lines (got {result.stdout!r})",
    )

    for label, fake_transport, expected in (
        ("healthy", "ssh_box() { return 0; }", 0),
        (
            "inactive service",
            "ssh_box() { case \"$1\" in systemctl*) return 3;; *) return 0;; esac; }",
            3,
        ),
        (
            "unhealthy endpoint",
            "ssh_box() { case \"$1\" in curl*) return 22;; *) return 0;; esac; }",
            1,
        ),
    ):
        result = run_sender_prefix(
            source,
            "300",
            f"{fake_transport}\nym_status >/dev/null\nexit $?",
        )
        require(
            result.returncode == expected,
            f"status {label} must return {expected} (rc={result.returncode})",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
