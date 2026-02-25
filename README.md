# Wintermute

A self-coding AI agent. Single Rust binary. Talks to you via Telegram.
Writes tools to extend itself. Privacy boundary: your data never leaves
without your consent.

Named after the AI in William Gibson's *Neuromancer* — the intelligence
that orchestrated its own evolution.

## Install

**Quick install** (macOS / Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/pycckuu/wintermute/main/install.sh | bash
```

**Pre-built binaries** — download from
[GitHub Releases](https://github.com/pycckuu/wintermute/releases):

| Platform | Target |
|----------|--------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |

**Build from source:**

```bash
cargo build --release
```

## Setup

```bash
wintermute init      # First-time setup
wintermute start     # Start the agent
wintermute status    # Health check
wintermute reset     # Recreate sandbox
wintermute backup    # Immediate backup
```

**Prerequisites:** Docker (recommended for sandboxed execution), a
Telegram bot token, and at least one LLM API key (Anthropic or OpenAI).

Configure in `~/.wintermute/config.toml` (see `config.example.toml`).

See `DESIGN.md` for full architecture documentation.

## Running with Flatline (supervisor)

Flatline monitors Wintermute and auto-fixes failures (restart on crash,
quarantine bad tools, revert broken changes).

```bash
# Recommended: Flatline starts Wintermute automatically
flatline start

# Or run both independently
wintermute start     # Terminal 1
flatline start       # Terminal 2
```

Configure in `~/.wintermute/flatline.toml` (see `flatline.toml.example`).
Set `start_on_boot = false` for monitoring-only mode.

See `doc/FLATLINE.md` for full supervisor documentation.

## Running as a systemd service (Linux)

```bash
cp systemd/wintermute.service ~/.config/systemd/user/
cp systemd/flatline.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wintermute
systemctl --user enable --now flatline
```

## License

Apache 2.0

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).
