# Queso

[![CI](https://github.com/jtdowney/queso/actions/workflows/ci.yml/badge.svg)](https://github.com/jtdowney/queso/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/queso)](https://crates.io/crates/queso)

Package Gleam applications into single native executables. The output binary bundles compiled BEAM bytecode and an Erlang runtime, so it runs on machines without Erlang installed.

Inspired by [Burrito](https://github.com/burrito-elixir/burrito), built for Gleam. Because sometimes you go out to get a burrito, but all you really want is the queso.

## Quick start

Inside a Gleam project:

```
queso build
```

That's it. Queso detects your platform, downloads a matching Erlang runtime, compiles your project, and produces a single executable in `build/queso/`. On Linux, the default target is `linux-static` (no libc dependency).

```
$ ./build/queso/my_app-1.0.0-aarch64-macos
Hello from my_app!
```

> [!NOTE]
> Queso does not currently validate checksums of downloaded ERTS archives. The prebuilt runtimes are sourced from multiple providers across many releases, making automated checksum verification impractical today. Queso does print the SHA-256 hash of each download so you can verify it manually. If this is a concern for your supply chain, download the ERTS release yourself and point to it with `--erts`.

## Install

### Prebuilt binaries

Download a prebuilt binary from the [latest release](https://github.com/jtdowney/queso/releases/latest), extract it, and place it somewhere on your PATH.

### Cargo (build from source)

With [Rust](https://rustup.rs) installed:

```sh
cargo install --locked queso
```

## Package time dependencies

`queso build` shells out to a handful of tools, all of which must be on your PATH:

- [Gleam](https://gleam.run) and [Erlang](https://www.erlang.org), to compile your app
- Rust (`cargo` and `rustc`, e.g. via [rustup](https://rustup.rs)), to build the launcher

On Linux, building for the default `linux-static` target additionally requires:

- The musl rustup target: `rustup target add $(uname -m)-unknown-linux-musl`
- A musl C toolchain (on Debian/Ubuntu: `sudo apt install musl-tools`)

Cross-compilation additionally requires:

- [Zig](https://ziglang.org)
- [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
- `rustup target add <rust-triple>` for each non-host target (for example `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`)

> [!NOTE]
> **Why Zig for cross-compilation?** Zig's cross-compilation toolchain makes it straightforward to compile the launcher binary for different target platforms.

## Usage

```
queso build [OPTIONS]
```

All flags are optional:

| Flag                      | Description                                                                                                      |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `--target <TARGET>`       | Target platform (repeatable; defaults to current platform)                                                       |
| `--erts <PATH>`           | Path to Erlang/OTP installation (auto-downloaded if omitted; cannot be combined with multiple `--target` values) |
| `--entry <MODULE>`        | Entrypoint module (defaults to package name)                                                                     |
| `--full-erts`             | Bundle the entire ERTS (skip tree shaking)                                                                       |
| `--strip-beam`            | Strip debug info from BEAM files (default)                                                                       |
| `--no-strip-beam`         | Keep debug info in BEAM files                                                                                    |
| `--compression-level <N>` | Zstd compression level, 1-22 (default: 9)                                                                        |

### Cross-compilation

Pass `--target` to build for a different platform. You can specify multiple targets in a single invocation:

```
queso build --target x86_64-linux-static --target aarch64-macos
```

Supported targets:

| Target                 | Description                              |
| ---------------------- | ---------------------------------------- |
| `aarch64-linux-glibc`  | Linux ARM64, dynamically linked (glibc)  |
| `aarch64-linux-musl`   | Linux ARM64, dynamically linked (musl)   |
| `aarch64-linux-static` | Linux ARM64, statically linked           |
| `aarch64-macos`        | macOS ARM64 (Apple Silicon)              |
| `x86_64-linux-glibc`   | Linux x86_64, dynamically linked (glibc) |
| `x86_64-linux-musl`    | Linux x86_64, dynamically linked (musl)  |
| `x86_64-linux-static`  | Linux x86_64, statically linked          |
| `x86_64-macos`         | macOS x86_64 (Intel)                     |
| `x86_64-windows`       | Windows x86_64                           |
| `aarch64-windows`      | Windows ARM64 (requires `--erts`)        |

Linux targets require a libc variant. The `static` variant is the most portable (no libc dependency) but does not export symbols needed for NIF dependencies to function. The `glibc` and `musl` variants include support for NIFs but require the corresponding libc on the target system.

> [!TIP]
> The word "musl" appears in two different contexts. The queso **release binaries** (e.g., `x86_64-unknown-linux-musl`) are statically linked Rust executables with no runtime dependencies. The queso **build targets** (e.g., `x86_64-linux-musl`) refer to the libc used by the bundled Erlang runtime, which is dynamically linked against musl.

Cross-compilation works because Zig (via cargo-zigbuild) handles cross-compiling the Rust launcher and queso downloads the correct Erlang runtime for the target platform.

> [!NOTE]
> Cross-compiling NIF (Native Implemented Function) dependencies from Hex is not currently supported. Projects with NIF dependencies should be built on the target platform. I have some ideas about how I would do this with `zig cc` so it may be supported in the future.

### Custom ERTS

By default, queso downloads a precompiled Erlang runtime matching your installed OTP version. To use a specific installation:

```
queso build --erts /opt/erlang/28.4.1
```

### Compression

The payload is compressed with zstd (with the default compression level set to 9). Higher levels produce smaller binaries but take longer to compress:

```
queso build --compression-level 19
```

### Configuration via gleam.toml

Most build options can be set in your `gleam.toml` under `[tools.queso]`:

```toml
[tools.queso]
entry = "my_app.cli"
targets = ["aarch64-macos", "x86_64-linux-static"]
strip_beam = false
compression_level = 3
full_erts = true
```

CLI flags, when provided, take precedence over `gleam.toml` values.

## How it works

1. Parses `gleam.toml` for project name, version, and config
2. Runs `gleam export erlang-shipment` to produce a minimal set of BEAM files
3. Downloads or locates a compatible Erlang runtime (ERTS)
4. Compiles a small Rust launcher binary for the target platform
5. Appends ERTS + BEAM files + metadata as a compressed payload to the launcher
6. The launcher, on first run, extracts the payload to a cache directory and boots the Erlang runtime

The resulting binary is fully self-contained. On first execution, it extracts to a versioned cache directory named after the project (`~/.cache/<project_name>/` on Linux, `~/Library/Caches/<project_name>/` on macOS). Subsequent runs reuse the cache.

## Alternatives

- [gleescript](https://github.com/lpil/gleescript) - bundles a Gleam project into a self-contained escript, requiring only Erlang on the target machine
- [mix_gleam](https://github.com/gleam-lang/mix_gleam) + [Burrito](https://github.com/burrito-elixir/burrito) - compile Gleam within a Mix project and use Burrito to produce a standalone executable
