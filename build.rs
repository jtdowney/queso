#[path = "src/pack.rs"]
mod pack;

use std::{env, fs};

use camino::Utf8PathBuf;
use eyre::{Result, WrapErr, bail};
use sha2::{Digest, Sha256};

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/pack.rs");

    let out_dir = Utf8PathBuf::from(env::var("OUT_DIR").wrap_err("OUT_DIR not set")?);
    let manifest_dir =
        Utf8PathBuf::from(env::var("CARGO_MANIFEST_DIR").wrap_err("CARGO_MANIFEST_DIR not set")?);
    let archive_path = out_dir.join("launcher-source.tar.zst");
    let launcher_dir = manifest_dir.join("launcher");

    if launcher_dir.join("Cargo.toml").exists() {
        for path in pack::source_trigger_paths(&manifest_dir) {
            println!("cargo:rerun-if-changed={path}");
        }
        pack::pack_launcher_source(&manifest_dir, &archive_path)?;
    } else {
        let committed = manifest_dir.join("launcher-source.tar.zst");
        println!("cargo:rerun-if-changed={committed}");
        if !committed.exists() {
            bail!(
                "launcher source archive missing at {committed}. \
                 if publishing from source, run `just pack-launcher` before `cargo publish`; \
                 if building from a git checkout, ensure the `launcher/` directory is present."
            );
        }
        fs::copy(&committed, &archive_path)
            .wrap_err_with(|| format!("failed to copy committed archive from {committed}"))?;
    }

    let bytes = fs::read(&archive_path)
        .wrap_err_with(|| format!("failed to read archive at {archive_path}"))?;
    let hash = hex::encode(Sha256::digest(&bytes));
    println!("cargo:rustc-env=QUESO_LAUNCHER_SOURCE_HASH={hash}");
    Ok(())
}
