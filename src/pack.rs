use std::{
    fs::{self, File},
    io::Write,
};

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Result, WrapErr, eyre};
use walkdir::WalkDir;

pub fn pack_launcher_source(manifest_dir: &Utf8Path, archive_path: &Utf8Path) -> Result<()> {
    let launcher_dir = manifest_dir.join("launcher");
    let shared_dir = manifest_dir.join("shared");
    let src_dir = launcher_dir.join("src");
    let cargo_toml = launcher_dir.join("Cargo.toml");
    let cargo_lock = launcher_dir.join("Cargo.lock");

    let file = File::create(archive_path)
        .wrap_err_with(|| format!("failed to create archive at {archive_path}"))?;
    let encoder = zstd::Encoder::new(file, 3).wrap_err("failed to initialize zstd encoder")?;
    let mut builder = tar::Builder::new(encoder);

    for root in [&cargo_toml, &cargo_lock] {
        let data = fs::read(root).wrap_err_with(|| format!("failed to read {root}"))?;
        let relative = root
            .strip_prefix(manifest_dir)
            .wrap_err_with(|| format!("entry {root} is not under manifest dir"))?;
        append_file(&mut builder, relative, &data)?;
    }

    for dir in [&src_dir, &shared_dir] {
        for entry in WalkDir::new(dir.as_str()).sort_by_file_name() {
            let entry = entry.wrap_err_with(|| format!("failed to walk {dir}"))?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = Utf8Path::from_path(entry.path())
                .ok_or_else(|| eyre!("path is not valid UTF-8: {}", entry.path().display()))?;
            let relative = path
                .strip_prefix(manifest_dir)
                .wrap_err_with(|| format!("entry {path} is not under manifest dir"))?;
            let data = fs::read(path).wrap_err_with(|| format!("failed to read {path}"))?;
            append_file(&mut builder, relative, &data)?;
        }
    }

    let encoder = builder
        .into_inner()
        .wrap_err("failed to finish tar archive")?;
    encoder.finish().wrap_err("failed to finish zstd stream")?;
    Ok(())
}

#[must_use]
pub fn source_trigger_paths(manifest_dir: &Utf8Path) -> [Utf8PathBuf; 4] {
    let launcher_dir = manifest_dir.join("launcher");
    [
        launcher_dir.join("Cargo.toml"),
        launcher_dir.join("Cargo.lock"),
        launcher_dir.join("src"),
        manifest_dir.join("shared"),
    ]
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
