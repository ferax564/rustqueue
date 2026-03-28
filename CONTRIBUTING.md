# Contributing to RustQueue

Thanks for your interest in contributing to RustQueue! This document covers the basics of getting started.

## Development Setup

### Prerequisites

- Rust 1.85+ (install via [rustup](https://rustup.rs/))
- Git

### Build & Test

```bash
cargo build                          # Debug build
cargo test                           # Run all tests (default features)
cargo test --features sqlite         # Include SQLite backend tests
cargo clippy                         # Lint
cargo fmt --check                    # Format check
```

### Before Submitting a PR

Every PR must pass all three checks:

```bash
cargo test                                        # All tests pass
cargo clippy --features sqlite,postgres,otel      # Zero warnings
cargo fmt --check                                 # Zero diffs
```

## Project Structure

```
src/
  api/        — HTTP REST API (axum)
  engine/     — Core business logic (queue, scheduler, webhooks, DAG flows)
  storage/    — Storage backends (redb, hybrid, memory, sqlite, postgres)
  protocol/   — TCP protocol (newline-delimited JSON + binary)
  config/     — Configuration loading
  dashboard/  — Embedded web dashboard
tests/        — Integration tests
benches/      — Criterion benchmarks
sdk/          — Client SDKs (Node.js, Python, Go)
```

## Coding Standards

- **Error handling**: `thiserror` for library errors, `anyhow` for binary/integration code
- **Async**: Everything async via tokio. Use `#[async_trait]` for trait objects
- **Naming**: snake_case for files/functions, PascalCase for types
- **Testing**: Unit tests in `#[cfg(test)]` modules, integration tests in `tests/`
- **No unsafe**: Unless absolutely necessary and well-documented

## PR Process

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes with tests
4. Run the full verification (test + clippy + fmt)
5. Submit a PR with a clear description of what and why

## Feature Flags

When adding code behind a feature flag, ensure it compiles with and without the flag:

```bash
cargo check                              # Default features
cargo check --features sqlite,postgres   # With optional backends
cargo check --no-default-features        # Minimal build
```

## Reporting Issues

- Use GitHub Issues for bug reports and feature requests
- Include reproduction steps for bugs
- Include your Rust version (`rustc --version`) and OS

## License

By contributing, you agree that your contributions will be licensed under the MIT OR Apache-2.0 dual license.
