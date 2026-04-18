default:
    @just --list

# Regenerate launcher-source.tar.zst at the repo root from launcher/ and shared/
pack-launcher:
    cargo xtask pack-launcher

# Dry-run cargo release (no publish, no tag)
release-dry-run: pack-launcher
    cargo release --allow-dirty

# Publish a new version, e.g. `just release 0.4.0`
release version: pack-launcher
    cargo release {{version}} --execute --allow-dirty
