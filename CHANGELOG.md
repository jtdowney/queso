# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-07

### Added

#### Build & Packaging

- Package Gleam applications into single native executables
- Bundle compiled BEAM bytecode and Erlang runtime into one binary
- Automatic ERTS download matching installed OTP version
- Custom ERTS path support via `--erts`
- Configurable zstd compression level (1-22, default 9)
- BEAM file debug info stripping (on by default)
- ERTS tree shaking to minimize binary size (`--full-erts` to disable)
- SHA-256 hash output for downloaded ERTS archives

#### Cross-Compilation

- Cross-compile via Zig for all supported targets
- Multi-target builds in a single invocation (`--target` repeatable)

#### Target Platforms

- `aarch64-macos`, `x86_64-macos`
- `aarch64-linux-glibc`, `aarch64-linux-musl`, `aarch64-linux-static`
- `x86_64-linux-glibc`, `x86_64-linux-musl`, `x86_64-linux-static`
- `x86_64-windows`, `aarch64-windows`

#### Configuration

- `gleam.toml` configuration under `[tools.queso]`
- CLI flags override `gleam.toml` values
- Custom entrypoint module via `--entry`

#### Runtime

- Zig launcher extracts payload to versioned cache directory on first run
- Subsequent runs reuse the cache

[0.1.0]: https://github.com/jtdowney/queso/releases/tag/v0.1.0
