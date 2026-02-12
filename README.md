# PFAR — Privacy-First Agent Runtime v2

A privacy-first, multi-channel AI agent runtime built as a single Rust binary (hardened monolith). Connects to messaging platforms (Telegram, Slack, WhatsApp), SaaS APIs (email, calendar, GitHub, Notion), browser automation, and scheduled jobs — while enforcing strict information-flow control so that no data leaks across users, channels, or providers without explicit owner authorization.

## Architecture

- **Single Rust binary** with in-process async tasks for adapters and tools
- **Policy Engine** enforcing mandatory access control lattice, taint propagation, and capability tokens
- **Plan-Then-Execute pipeline** with structural separation of planning from content exposure
- Containers reserved **only** for browser service and script runner

See [docs/pfar-v2-spec.md](docs/pfar-v2-spec.md) for the full system design specification.

## Building

```sh
cargo build
```

## Running

```sh
cargo run
```

## Testing

```sh
cargo test
```

## License

Apache 2.0

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).
