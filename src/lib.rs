pub mod erl;
pub mod erts;
pub mod payload;
pub mod project;
pub mod strip;
pub mod target;
pub mod tree_shake;

use std::{fmt::Write, fs, io, process::Command};

use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile::Utf8TempDir;
use directories::ProjectDirs;
use eyre::{Context, Result, bail, ensure, eyre};
use sha2::{Digest, Sha256};

use crate::target::Target;

pub const LAUNCHER_SOURCE: &str = include_str!("../launcher/main.zig");

pub fn cache_dir() -> Result<Utf8PathBuf> {
    let proj_dirs = ProjectDirs::from("", "", "queso")
        .ok_or_else(|| eyre!("could not determine cache directory"))?;
    let dir = Utf8Path::from_path(proj_dirs.cache_dir())
        .ok_or_else(|| eyre!("non-UTF-8 cache directory"))?;
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

#[derive(Debug)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    pub entry_module: String,
    pub target: Target,
    pub erts_version: String,
    pub erts_hash: String,
    pub app_hash: String,
    pub boot_path: String,
}

impl Metadata {
    #[must_use]
    pub fn to_zig(&self) -> String {
        let constants = [
            ("name", self.name.as_str()),
            ("version", self.version.as_str()),
            ("entry_module", self.entry_module.as_str()),
            ("target", &self.target.to_string()),
            ("erts_version", self.erts_version.as_str()),
            ("erts_hash", self.erts_hash.as_str()),
            ("app_hash", self.app_hash.as_str()),
            ("boot_path", self.boot_path.as_str()),
        ];

        constants
            .into_iter()
            .fold(String::new(), |mut out, (key, value)| {
                let _ = writeln!(out, "pub const {key} = \"{}\";", escape_zig_string(value));
                out
            })
    }
}

fn escape_zig_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out
}

pub fn check_zig() -> Result<String> {
    let output = Command::new("zig")
        .arg("version")
        .output()
        .map_err(|_| eyre::eyre!("zig not found on PATH"))?;

    ensure!(
        output.status.success(),
        "zig version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn check_gleam() -> Result<String> {
    let output = Command::new("gleam")
        .arg("--version")
        .output()
        .map_err(|_| eyre::eyre!("gleam not found on PATH"))?;

    ensure!(output.status.success(), "gleam --version failed");

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches("gleam ")
        .to_string())
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

pub fn compile_launcher(
    target: &Target,
    erts_payload_path: impl AsRef<Utf8Path>,
    app_payload_path: impl AsRef<Utf8Path>,
    metadata: &Metadata,
    output_path: impl AsRef<Utf8Path>,
) -> Result<()> {
    let erts_payload_path = erts_payload_path.as_ref();
    let app_payload_path = app_payload_path.as_ref();
    let output_path = output_path.as_ref();
    let work_dir = Utf8TempDir::new().wrap_err("failed to create temp dir for Zig compilation")?;
    let output_dir = output_path
        .parent()
        .ok_or_else(|| eyre::eyre!("output path has no parent directory: {output_path}"))?;

    fs::write(work_dir.path().join("main.zig"), LAUNCHER_SOURCE)?;
    fs::copy(erts_payload_path, work_dir.path().join("erts.tar.zst"))?;
    fs::copy(app_payload_path, work_dir.path().join("app.tar.zst"))?;
    fs::write(work_dir.path().join("config.zig"), metadata.to_zig())?;

    let tmp_path = camino_tempfile::Builder::new().make_in(output_dir, |tmp_path| {
        let mut cmd = Command::new("zig");
        cmd.arg("build-exe")
            .arg("main.zig")
            .args(["--name", "queso-launcher"])
            .args(["-target", &target.zig_target()])
            .arg(format!("-femit-bin={tmp_path}"))
            .arg("-OReleaseSmall")
            .current_dir(work_dir.path());

        let result = cmd.output()?;
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(io::Error::other(format!(
                "zig compilation failed:\n{stderr}"
            )));
        }

        Ok(())
    })?;

    tmp_path
        .persist(output_path)
        .wrap_err_with(|| format!("failed to place binary at {output_path}"))?;

    Ok(())
}

#[must_use]
pub fn output_filename(name: &str, version: &str, target: &Target) -> String {
    format!("{name}-{version}-{target}{}", target.exe_suffix())
}

#[cfg(test)]
mod test {
    use camino_tempfile_ext::prelude::*;

    use super::*;

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
    fn test_validate_entrypoint_no_build_dir() {
        let dir = camino_tempfile::tempdir().unwrap();
        assert!(gleam_validate_entrypoint(dir.path(), "my_app.beam").is_err());
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
    fn test_find_boot_file_no_releases_dir() {
        let dir = camino_tempfile::tempdir().unwrap();
        assert!(find_boot_file(dir.path()).is_err());
    }

    #[test]
    fn test_output_filename() {
        let target = "x86_64-linux-static".parse::<Target>().unwrap();
        assert_eq!(
            output_filename("my_app", "1.2.3", &target),
            "my_app-1.2.3-x86_64-linux-static"
        );
    }

    #[test]
    fn test_output_filename_windows() {
        let target = "x86_64-windows".parse::<Target>().unwrap();
        assert_eq!(
            output_filename("my_app", "1.0.0", &target),
            "my_app-1.0.0-x86_64-windows.exe"
        );
    }

    #[test]
    fn test_metadata_zig_snapshot() {
        let metadata = Metadata {
            name: "my_app".into(),
            version: "1.2.3".into(),
            entry_module: "my_app@cli".into(),
            target: "x86_64-linux-static".parse::<Target>().unwrap(),
            erts_version: "15.2.1".into(),
            erts_hash: "abc1234567890def".into(),
            app_hash: "0123456789abcdef".into(),
            boot_path: "releases/28/no_dot_erlang".into(),
        };
        insta::assert_snapshot!(metadata.to_zig(), @r#"
        pub const name = "my_app";
        pub const version = "1.2.3";
        pub const entry_module = "my_app@cli";
        pub const target = "x86_64-linux-static";
        pub const erts_version = "15.2.1";
        pub const erts_hash = "abc1234567890def";
        pub const app_hash = "0123456789abcdef";
        pub const boot_path = "releases/28/no_dot_erlang";
        "#);
    }

    #[test]
    fn test_metadata_zig_escapes_special_chars() {
        let metadata = Metadata {
            name: "app\"with\\quotes".into(),
            version: "1.0\n.0".into(),
            entry_module: "app\t\r".into(),
            target: "x86_64-linux-static".parse::<Target>().unwrap(),
            erts_version: "15.0".into(),
            erts_hash: "abc1234567890def".into(),
            app_hash: "0123456789abcdef".into(),
            boot_path: "releases/28/no_dot_erlang".into(),
        };
        insta::assert_snapshot!(metadata.to_zig(), @r#"
        pub const name = "app\"with\\quotes";
        pub const version = "1.0\n.0";
        pub const entry_module = "app\t\r";
        pub const target = "x86_64-linux-static";
        pub const erts_version = "15.0";
        pub const erts_hash = "abc1234567890def";
        pub const app_hash = "0123456789abcdef";
        pub const boot_path = "releases/28/no_dot_erlang";
        "#);
    }
}
