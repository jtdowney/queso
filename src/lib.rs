pub mod erl;
pub mod erts;
#[path = "../shared/format.rs"]
pub mod format;
#[doc(hidden)]
pub mod pack;
pub mod payload;
pub mod project;
pub mod strip;
pub mod target;
pub mod tree_shake;

use std::{
    fs::{self, File},
    io::{self, Seek, Write},
    process::Command,
};

use camino::{Utf8Path, Utf8PathBuf};
use directories::ProjectDirs;
use eyre::{Context, OptionExt, Result, bail, ensure, eyre};
use sha2::{Digest, Sha256};

use crate::{format::Trailer, target::Target};

const LAUNCHER_SOURCE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/launcher-source.tar.zst"));
const LAUNCHER_SOURCE_HASH: &str = env!("QUESO_LAUNCHER_SOURCE_HASH");
const QUESO_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn cache_dir() -> Result<Utf8PathBuf> {
    let proj_dirs =
        ProjectDirs::from("", "", "queso").ok_or_eyre("could not determine cache directory")?;
    let dir = Utf8Path::from_path(proj_dirs.cache_dir()).ok_or_eyre("non-UTF-8 cache directory")?;
    Ok(dir.to_path_buf())
}

pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    pub fn finalize(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

impl<W: io::Write> io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub use crate::format::Metadata;

pub fn check_gleam() -> Result<String> {
    let output = Command::new("gleam")
        .arg("--version")
        .output()
        .wrap_err("gleam not found on PATH")?;

    ensure!(output.status.success(), "gleam --version failed");

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches("gleam ")
        .to_string())
}

pub fn check_zig() -> Result<String> {
    if let Ok(output) = Command::new("zig").arg("version").output()
        && output.status.success()
    {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    if let Ok(output) = Command::new("python3")
        .args(["-m", "ziglang", "version"])
        .output()
        && output.status.success()
    {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    bail!(
        "zig not found (install from https://ziglang.org/download/ or with: pip install ziglang)"
    );
}

pub fn check_cargo_zigbuild() -> Result<String> {
    let output = Command::new("cargo-zigbuild")
        .arg("--version")
        .output()
        .wrap_err("cargo-zigbuild not found (install with: cargo install cargo-zigbuild)")?;

    ensure!(
        output.status.success(),
        "cargo-zigbuild --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches("cargo-zigbuild ")
        .to_string())
}

#[must_use]
pub fn is_cross_target(target: &Target) -> bool {
    Target::current().is_ok_and(|current| current != *target)
}

pub fn gleam_build(project_root: impl AsRef<Utf8Path>) -> Result<()> {
    let project_root = project_root.as_ref();
    let output = Command::new("gleam")
        .args(["export", "erlang-shipment"])
        .current_dir(project_root)
        .output()
        .wrap_err("failed to execute gleam export")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gleam export erlang-shipment failed:\n{stderr}");
    }

    Ok(())
}

#[must_use]
pub fn gleam_erlang_build_dir(project_root: impl AsRef<Utf8Path>) -> Utf8PathBuf {
    project_root.as_ref().join("build").join("erlang-shipment")
}

pub fn gleam_validate_entrypoint(
    project_root: impl AsRef<Utf8Path>,
    beam_file: &str,
) -> Result<()> {
    let project_root = project_root.as_ref();
    let erlang_dir = gleam_erlang_build_dir(project_root);

    for entry in erlang_dir
        .read_dir_utf8()
        .wrap_err_with(|| format!("build output not found at {erlang_dir}"))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let beam_path = entry.path().join("ebin").join(beam_file);
            if beam_path.exists() {
                return Ok(());
            }
        }
    }

    bail!("entrypoint '{beam_file}' not found in build output at {erlang_dir}");
}

pub fn find_boot_file(erts_root: impl AsRef<Utf8Path>) -> Result<String> {
    let erts_root = erts_root.as_ref();
    let releases_dir = erts_root.join("releases");

    ensure!(
        releases_dir.is_dir(),
        "releases directory not found in ERTS at {erts_root}"
    );

    for entry in releases_dir
        .read_dir_utf8()
        .wrap_err_with(|| format!("failed to read {releases_dir}"))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let boot_file = entry.path().join("no_dot_erlang.boot");
            if boot_file.exists() {
                let relative = boot_file
                    .strip_prefix(erts_root)
                    .wrap_err("failed to compute relative boot path")?;
                let without_ext = relative.with_extension("");
                return Ok(without_ext.into_string());
            }
        }
    }

    bail!("no_dot_erlang.boot not found in {releases_dir}");
}

pub fn ensure_launcher(target: &Target) -> Result<Utf8PathBuf> {
    let cache_dir = cache_dir()?;
    let rust_target = target.rust_target();
    let key = format!("{QUESO_VERSION}-{}", &LAUNCHER_SOURCE_HASH[..12]);
    let launcher_dir = cache_dir.join("launcher").join(&key).join(&rust_target);
    let suffix = target.exe_suffix();
    let launcher_path = launcher_dir.join(format!("queso-launcher{suffix}"));

    if launcher_path.is_file() {
        return Ok(launcher_path);
    }

    let build_dir = cache_dir.join("launcher-build").join(&key);
    ensure_launcher_source(&build_dir)?;

    let cross = is_cross_target(target);
    let manifest_path = build_dir.join("launcher").join("Cargo.toml").into_string();
    let subcommand = if cross { "zigbuild" } else { "build" };

    let output = Command::new("cargo")
        .args([
            subcommand,
            "--locked",
            "--release",
            "--target",
            &rust_target,
            "--manifest-path",
            &manifest_path,
        ])
        .output()
        .wrap_err_with(|| {
            if cross {
                "failed to run cargo zigbuild (install with: cargo install cargo-zigbuild)"
                    .to_string()
            } else {
                "failed to run cargo build".to_string()
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("launcher compilation failed:\n{stderr}");
    }

    let built_binary = build_dir
        .join("launcher")
        .join("target")
        .join(&rust_target)
        .join("release")
        .join(format!("queso-launcher{suffix}"));

    ensure!(
        built_binary.is_file(),
        "compiled launcher not found at {built_binary}"
    );

    fs::create_dir_all(&launcher_dir)?;
    let tmp_path = camino_tempfile::Builder::new().make_in(&launcher_dir, |tmp_path| {
        fs::copy(&built_binary, tmp_path).map(|_| ())
    })?;
    tmp_path
        .persist(&launcher_path)
        .wrap_err_with(|| format!("failed to cache launcher at {launcher_path}"))?;

    Ok(launcher_path)
}

fn ensure_launcher_source(build_dir: &Utf8Path) -> Result<()> {
    if build_dir.is_dir() {
        return Ok(());
    }

    let parent = build_dir
        .parent()
        .ok_or_eyre("launcher build dir has no parent")?;
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("failed to create launcher build parent {parent}"))?;

    let tmp_dir = camino_tempfile::Builder::new()
        .prefix("extracting.")
        .tempdir_in(parent)
        .wrap_err("failed to create temp dir for launcher source")?;

    extract_launcher_source(tmp_dir.path())?;

    let extracted = tmp_dir.keep();
    match fs::rename(&extracted, build_dir) {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
            ) && build_dir.is_dir() =>
        {
            fs::remove_dir_all(&extracted).ok();
            Ok(())
        }
        Err(err) => {
            fs::remove_dir_all(&extracted).ok();
            Err(err).wrap_err_with(|| format!("failed to finalize launcher source at {build_dir}"))
        }
    }
}

fn extract_launcher_source(dest: &Utf8Path) -> Result<()> {
    let decoder =
        zstd::Decoder::new(LAUNCHER_SOURCE).wrap_err("failed to decompress launcher source")?;
    let mut archive = tar::Archive::new(decoder);

    for entry in archive
        .entries()
        .wrap_err("failed to read launcher source archive")?
    {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let dest_path = dest.as_std_path().join(&path);

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if entry.header().entry_type().is_file() {
            let mut file = File::create(&dest_path)?;
            io::copy(&mut entry, &mut file)?;
        }
    }

    Ok(())
}

pub fn assemble_binary(
    launcher_path: impl AsRef<Utf8Path>,
    erts_payload_path: impl AsRef<Utf8Path>,
    app_payload_path: impl AsRef<Utf8Path>,
    metadata: &Metadata,
    output_path: impl AsRef<Utf8Path>,
) -> Result<()> {
    let launcher_path = launcher_path.as_ref();
    let erts_payload_path = erts_payload_path.as_ref();
    let app_payload_path = app_payload_path.as_ref();
    let output_path = output_path.as_ref();

    let output_dir = output_path
        .parent()
        .ok_or_else(|| eyre!("output path has no parent directory: {output_path}"))?;

    let tmp_path = camino_tempfile::Builder::new().make_in(output_dir, |tmp_path| {
        assemble_binary_inner(
            launcher_path,
            erts_payload_path,
            app_payload_path,
            metadata,
            tmp_path.as_str(),
        )
        .map_err(|e| io::Error::other(e.to_string()))
    })?;

    tmp_path
        .persist(output_path)
        .wrap_err_with(|| format!("failed to place binary at {output_path}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output_path, fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

fn assemble_binary_inner(
    launcher_path: &Utf8Path,
    erts_payload_path: &Utf8Path,
    app_payload_path: &Utf8Path,
    metadata: &Metadata,
    output_path: &str,
) -> Result<()> {
    let mut out = File::create(output_path)?;

    io::copy(&mut File::open(launcher_path)?, &mut out)?;

    let erts = out.stream_position()?;
    io::copy(&mut File::open(erts_payload_path)?, &mut out)?;

    let app = out.stream_position()?;
    io::copy(&mut File::open(app_payload_path)?, &mut out)?;

    let meta = out.stream_position()?;
    let meta_bytes = bincode::encode_to_vec(metadata, bincode::config::standard())
        .wrap_err("failed to serialize metadata")?;
    out.write_all(&meta_bytes)?;

    let trailer = Trailer {
        erts_offset: erts,
        app_offset: app,
        meta_offset: meta,
    };
    trailer.write(&mut out)?;

    Ok(())
}

#[must_use]
pub fn output_filename(name: &str, version: &str, target: &Target) -> String {
    format!("{name}-{version}-{target}{}", target.exe_suffix())
}

#[cfg(test)]
mod test {
    use camino_tempfile_ext::prelude::*;
    use quickcheck_macros::quickcheck;

    use super::*;
    use crate::format::{TRAILER_MAGIC, TRAILER_SIZE};

    #[test]
    fn test_validate_entrypoint_found() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("build/erlang-shipment/my_app/ebin/my_app.beam")
            .write_binary(b"BEAM")
            .unwrap();

        assert!(gleam_validate_entrypoint(dir.path(), "my_app.beam").is_ok());
    }

    #[test]
    fn test_validate_entrypoint_not_found() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("build/erlang-shipment/my_app/ebin/my_app.beam")
            .write_binary(b"BEAM")
            .unwrap();

        let err = gleam_validate_entrypoint(dir.path(), "other.beam").unwrap_err();
        insta::with_settings!({
            filters => vec![(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]")]
        }, {
            insta::assert_snapshot!(err.to_string(), @"entrypoint 'other.beam' not found in build output at [TEMPDIR]/build/erlang-shipment");
        });
    }

    #[test]
    fn test_find_boot_file() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("releases/28/no_dot_erlang.boot")
            .write_binary(b"boot")
            .unwrap();

        let path = find_boot_file(dir.path()).unwrap();
        assert_eq!(path, "releases/28/no_dot_erlang");
    }

    #[test]
    fn test_find_boot_file_missing() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("releases/28").create_dir_all().unwrap();

        let err = find_boot_file(dir.path()).unwrap_err();
        insta::with_settings!({
            filters => vec![(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]")]
        }, {
            insta::assert_snapshot!(err.to_string(), @"no_dot_erlang.boot not found in [TEMPDIR]/releases");
        });
    }

    #[test]
    fn test_output_filename() {
        let cases = [
            ("my_app", "1.2.3", "x86_64-linux-static"),
            ("my_app", "1.0.0", "x86_64-windows"),
            ("my_app", "0.1.0", "aarch64-macos"),
        ];
        let report: String = cases
            .iter()
            .map(|(name, version, target_str)| {
                let target = target_str.parse::<Target>().unwrap();
                format!(
                    "{} -> {}",
                    target_str,
                    output_filename(name, version, &target)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(report, @r"
        x86_64-linux-static -> my_app-1.2.3-x86_64-linux-static
        x86_64-windows -> my_app-1.0.0-x86_64-windows.exe
        aarch64-macos -> my_app-0.1.0-aarch64-macos
        ");
    }

    #[quickcheck]
    fn test_hashing_writer_matches_direct_hash(chunks: Vec<Vec<u8>>) -> bool {
        let concatenated: Vec<u8> = chunks.iter().flatten().copied().collect();
        let mut writer = HashingWriter::new(Vec::new());
        for chunk in chunks {
            writer.write_all(&chunk).unwrap();
        }
        writer.finalize() == hex::encode(Sha256::digest(&concatenated))
    }

    #[test]
    fn test_assemble_binary_creates_valid_output() {
        let dir = camino_tempfile::tempdir().unwrap();

        dir.child("launcher").write_binary(b"FAKE_EXE").unwrap();
        dir.child("erts.tar.zst")
            .write_binary(b"ERTS_DATA")
            .unwrap();
        dir.child("app.tar.zst").write_binary(b"APP_DATA").unwrap();

        let metadata = Metadata {
            name: "my_app".into(),
            version: "1.2.3".into(),
            entry_module: "my_app@cli".into(),
            erts_version: "15.2.1".into(),
            erts_hash: "abc123".into(),
            app_hash: "def456".into(),
            boot_path: "releases/28/no_dot_erlang".into(),
        };

        let output = dir.path().join("output");
        assemble_binary(
            dir.path().join("launcher"),
            dir.path().join("erts.tar.zst"),
            dir.path().join("app.tar.zst"),
            &metadata,
            &output,
        )
        .unwrap();

        let data = fs::read(output.as_std_path()).unwrap();

        assert!(data.starts_with(b"FAKE_EXE"));

        let trailer = &data[data.len() - TRAILER_SIZE..];
        assert_eq!(&trailer[24..32], TRAILER_MAGIC);

        let erts_offset =
            usize::try_from(u64::from_le_bytes(trailer[0..8].try_into().unwrap())).unwrap();
        let app_offset =
            usize::try_from(u64::from_le_bytes(trailer[8..16].try_into().unwrap())).unwrap();
        let meta_offset =
            usize::try_from(u64::from_le_bytes(trailer[16..24].try_into().unwrap())).unwrap();

        assert_eq!(erts_offset, b"FAKE_EXE".len());
        assert_eq!(&data[erts_offset..app_offset], b"ERTS_DATA");
        assert_eq!(&data[app_offset..meta_offset], b"APP_DATA");

        let meta_bytes = &data[meta_offset..data.len() - TRAILER_SIZE];
        let (parsed, _): (Metadata, _) =
            bincode::decode_from_slice(meta_bytes, bincode::config::standard()).unwrap();
        assert_eq!(parsed.name, "my_app");
        assert_eq!(parsed.erts_hash, "abc123");
    }
}
