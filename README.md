# Wintermute

A self-coding AI agent. Single Rust binary. Talks to you via Telegram.
Writes tools to extend itself. Privacy boundary: your data never leaves
without your consent.

Named after the AI in William Gibson's *Neuromancer* — the intelligence
that orchestrated its own evolution.

## Build

```bash
cargo build --release
```

## Setup

```bash
./wintermute init      # First-time setup
./wintermute start     # Start the agent
./wintermute status    # Health check
./wintermute reset     # Recreate sandbox
./wintermute backup    # Immediate backup
```

See `DESIGN.md` for full architecture documentation.

## Running with Flatline (supervisor)

Flatline monitors Wintermute and auto-fixes failures (restart on crash,
quarantine bad tools, revert broken changes).

```bash
# Recommended: Flatline starts Wintermute automatically
./flatline start

# Or run both independently
./wintermute start     # Terminal 1
./flatline start       # Terminal 2
```

Configure in `~/.wintermute/flatline.toml` (see `flatline.toml.example`).
Set `start_on_boot = false` for monitoring-only mode.

See `doc/FLATLINE.md` for full supervisor documentation.

## License

Apache 2.0

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).
