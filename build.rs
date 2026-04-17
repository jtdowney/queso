use std::{
    env,
    fs::{self, File},
    io::Write,
};

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Result, WrapErr, eyre};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = Utf8PathBuf::from(env::var("OUT_DIR").wrap_err("OUT_DIR not set")?);
    let manifest_dir =
        Utf8PathBuf::from(env::var("CARGO_MANIFEST_DIR").wrap_err("CARGO_MANIFEST_DIR not set")?);
    let launcher_dir = manifest_dir.join("launcher");
    let src_dir = launcher_dir.join("src");
    let cargo_toml = launcher_dir.join("Cargo.toml");
    let cargo_lock = launcher_dir.join("Cargo.lock");
    println!("cargo:rerun-if-changed={cargo_toml}");
    println!("cargo:rerun-if-changed={cargo_lock}");
    println!("cargo:rerun-if-changed={src_dir}");

    let archive_path = out_dir.join("launcher-source.tar.zst");
    let file = File::create(&archive_path)
        .wrap_err_with(|| format!("failed to create archive at {archive_path}"))?;
    let encoder = zstd::Encoder::new(file, 3).wrap_err("failed to initialize zstd encoder")?;
    let mut builder = tar::Builder::new(encoder);

    for root in [&cargo_toml, &cargo_lock] {
        let data = fs::read(root).wrap_err_with(|| format!("failed to read {root}"))?;
        let relative = root
            .strip_prefix(&launcher_dir)
            .wrap_err_with(|| format!("entry {root} is not under launcher"))?;
        append_file(&mut builder, relative, &data)?;
    }

    for entry in WalkDir::new(src_dir.as_str()).sort_by_file_name() {
        let entry = entry.wrap_err("failed to walk launcher src")?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = Utf8Path::from_path(entry.path())
            .ok_or_else(|| eyre!("path is not valid UTF-8: {}", entry.path().display()))?;
        let relative = path
            .strip_prefix(&launcher_dir)
            .wrap_err_with(|| format!("entry {path} is not under launcher"))?;
        let data = fs::read(path).wrap_err_with(|| format!("failed to read {path}"))?;
        append_file(&mut builder, relative, &data)?;
    }

    let encoder = builder
        .into_inner()
        .wrap_err("failed to finish tar archive")?;
    encoder.finish().wrap_err("failed to finish zstd stream")?;

    let bytes = fs::read(&archive_path)
        .wrap_err_with(|| format!("failed to re-read archive at {archive_path}"))?;
    let hash = hex::encode(Sha256::digest(&bytes));
    println!("cargo:rustc-env=QUESO_LAUNCHER_SOURCE_HASH={hash}");
    Ok(())
}

fn append_file(builder: &mut tar::Builder<impl Write>, path: &Utf8Path, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, data)
        .wrap_err_with(|| format!("failed to append {path} to archive"))?;
    Ok(())
}
