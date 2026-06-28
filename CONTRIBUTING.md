# Contributing to yantrik-mind

## Build

Rust 1.91, edition 2021, multi-thread tokio. Both sibling repos must be present (workspace path-deps point at `../yantrik-companion`):

```
cargo build -p mind-core       # build the orchestrator binary
cargo build                    # build the full workspace
```

**Do not cross-compile.** The `imap`/`lettre` crates link `native-tls` (OpenSSL); build on the target Linux host.

## Test

```
cargo test
```

Key test suites:
- **harm-gate** — property tests + a checked-in jailbreak/injection corpus; any failure is a build break. Do not modify `crates/mind-governance`.
- **memory** — DB-style tests against `:memory:` SQLite (belief posteriors, contradiction, consolidation).
- **orchestration** — uses a `ScriptedLLM` backend so ~90% of the path is deterministic.

Add or update a test when changing observable behaviour. Golden-transcript replay lives in `mind-evals`.

## Deploy

The mind runs on a Linux CT/VM (no GPU needed — inference is API-based). Source is shipped by rsync; build happens on the box.

**1. Ship source (from dev machine):**
```
rsync -az --delete --exclude target --exclude '*.db' \
  /path/to/yantrik-mind/       user@HOST:~/codes/yantrik-mind/
rsync -az --delete --exclude target \
  /path/to/yantrik-companion/  user@HOST:~/codes/yantrik-companion/
```

**2. Build + install (on the box):**
```
cd ~/codes/yantrik-mind && bash deploy/build_on_box.sh
```
This installs system deps, runs `cargo build --release -p mind-core`, creates the `yantrikmind` service user, installs the systemd unit, and seeds `/etc/yantrik-mind.env`.

**3. Set secrets (on the box):**
```
sudo editor /etc/yantrik-mind.env   # chmod 600 — fill NANOGPT_KEY, YM_TELEGRAM_TOKEN, etc.
sudo systemctl restart yantrik-mind
journalctl -u yantrik-mind -f
```
See `deploy/yantrik-mind.env.example` for required variables.

**4. Stop the dev instance** once the box is live — only one process may poll the Telegram token.

**Updating:** re-run steps 1–2 then `sudo systemctl restart yantrik-mind`. The unit sets `Restart=always`; the binary swap is safe.

For full rationale see [BUILD.md](BUILD.md) and [deploy/DEPLOY.md](deploy/DEPLOY.md).
