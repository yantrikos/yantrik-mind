#!/usr/bin/env python3
"""Static regression checks for the builder-selection boundary in self-build scripts."""

from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parent


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def main() -> int:
    improve = (ROOT / "self_improve.sh").read_text(encoding="utf-8")
    tick = (ROOT / "self_build_tick.sh").read_text(encoding="utf-8")

    require(
        improve.count('CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN') == 1,
        "self_improve must require Claude OAuth only in the Claude auth branch",
    )
    auth_case = improve.index('case "${YM_BUILDER:-claude}" in')
    auth_end = improve.index("esac", auth_case)
    auth_block = improve[auth_case:auth_end]
    for builder in ("qwen)", "codex)", "claude)"):
        require(builder in auth_block, f"self_improve auth case is missing {builder}")
    require(
        auth_block.index("qwen)")
        < auth_block.index("QWEN_API_KEY")
        < auth_block.index("codex)"),
        "Qwen authentication is outside the Qwen branch",
    )
    require(
        auth_block.index("codex)")
        < auth_block.index("auth.json")
        < auth_block.index("claude)"),
        "Codex authentication is outside the Codex branch",
    )
    require(
        auth_block.index("claude)") < auth_block.index("CLAUDE_CODE_OAUTH_TOKEN"),
        "Claude OAuth is required before builder selection",
    )
    require(
        'export CODEX_HOME="$CODEX_AUTH_HOME"' in improve,
        "self_improve must preserve the preflighted Codex auth path after isolating HOME",
    )

    qwen_build = improve.index('if [ "${YM_BUILDER:-claude}" = "qwen" ]')
    require(
        qwen_build
        < improve.index("export ANTHROPIC_AUTH_TOKEN", qwen_build)
        < improve.index('BUILD_JSON="$', qwen_build),
        "Qwen credentials must be exported before the builder command substitution",
    )

    require(
        tick.count('CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN') == 1,
        "self_build_tick must not re-require Claude OAuth after builder-aware preflight",
    )
    require(
        'CODEX_AUTH_HOME="${CODEX_HOME:-${HOME:-/root}/.codex}"' in tick,
        "self_build_tick must honor an explicit Codex auth directory",
    )
    require(
        'export CODEX_HOME="$CODEX_AUTH_HOME"' in tick,
        "Codex goal generation must preserve its auth path after isolating HOME",
    )
    quota_if = tick.index('if [ "${YM_BUILDER:-claude}" = "claude" ]; then')
    quota_curl = tick.index("api.anthropic.com/api/oauth/usage", quota_if)
    quota_else = tick.index("\nelse\n", quota_curl)
    require(
        quota_if < quota_curl < quota_else,
        "Claude quota lookup must stay inside the Claude-only branch",
    )
    require(
        'codex exec --skip-git-repo-check --sandbox read-only "$GOAL_PROMPT"' in tick,
        "Codex-selected ticks must use Codex for read-only goal generation",
    )
    require(
        'claude -p "$GOAL_PROMPT"' in tick,
        "Claude/Qwen goal generation path disappeared",
    )

    require("BUILD_RC=0" in improve, "builder exit status is not initialized")
    require(
        improve.count("BUILD_RC=$?") == 3,
        "every builder lane must capture its process exit status",
    )
    abort = improve.index("ABORT-BUILDER:")
    stage = improve.index("git add -A")
    require(abort < stage, "a failed builder could stage or propose its partial edits")
    require(
        "IMPROVE_RC=$?" in tick and 'grep -q "ABORT-BUILDER:"' in tick,
        "the tick cannot distinguish and preserve a pre-staging builder failure",
    )

    print("self-build builder isolation: ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"self-build builder isolation: FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
