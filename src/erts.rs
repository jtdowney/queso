use std::{
    fs::{self, File},
    io::{self, Read},
    path::{self, PathBuf},
};

use camino::{Utf8Path, Utf8PathBuf};
use directories::ProjectDirs;
use eyre::{Context, Result, bail, ensure, eyre};

use crate::{
    HashingWriter,
    target::{Arch, Libc, Os, Target},
};

#[derive(Debug)]
pub struct Erts {
    pub root: Utf8PathBuf,
    pub version: String,
    pub erts_dir: Utf8PathBuf,
}

pub enum ErtsResolution {
    Cached {
        path: Utf8PathBuf,
        otp_version: String,
    },
    Downloaded {
        path: Utf8PathBuf,
        otp_version: String,
        url: String,
        hash: String,
    },
}

pub fn validate(root: impl AsRef<Utf8Path>, target: &Target) -> Result<Erts> {
    let root = root.as_ref();
    ensure!(root.is_dir(), "ERTS path does not exist: {root}");

    let erts_dir =
        find_erts_dir(root).wrap_err_with(|| format!("invalid ERTS installation at {root}"))?;

    let version = erts_dir
        .file_name()
        .and_then(|n| n.strip_prefix("erts-"))
        .ok_or_else(|| eyre!("could not parse ERTS version"))?
        .to_string();

    let suffix = target.exe_suffix();
    let bin_dir = erts_dir.join("bin");
    let has_erl = bin_dir.join(format!("erl{suffix}")).exists()
        || bin_dir.join(format!("erlexec{suffix}")).exists();
    ensure!(has_erl, "missing erl/erlexec binary in {bin_dir}");

    let lib_dir = root.join("lib");
    ensure!(
        lib_dir.is_dir(),
        "missing lib/ directory in ERTS installation"
    );

    Ok(Erts {
        root: root.to_path_buf(),
        version,
        erts_dir,
    })
}

fn find_erts_dir(root: impl AsRef<Utf8Path>) -> Result<Utf8PathBuf> {
    let root = root.as_ref();
    let mut candidates = Vec::new();
    for entry in root.read_dir_utf8()? {
        let entry = entry?;
        if entry.file_name().starts_with("erts-") && entry.file_type()?.is_dir() {
            candidates.push(entry);
        }
    }

    match candidates.as_slice() {
        [] => bail!("no erts-* directory found"),
        [entry] => Ok(entry.path().into()),
        _ => bail!("multiple erts-* directories found"),
    }
}

pub fn resolve(otp_version: impl Into<String>, target: &Target) -> Result<ErtsResolution> {
    let otp_version = otp_version.into();
    let cache_dir = cache_dir_for(&otp_version, *target)?;

    if validate(&cache_dir, target).is_ok() {
        return Ok(ErtsResolution::Cached {
            path: cache_dir,
            otp_version,
        });
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| eyre!("cache dir has no parent"))?;

    fs::create_dir_all(parent).wrap_err("failed to create cache parent dir")?;

    let tmp_dir = camino_tempfile::Builder::new()
        .prefix("extracting.")
        .tempdir_in(parent)
        .wrap_err("failed to create temp dir for ERTS extraction")?;

    let url = download_url(&otp_version, target)?;
    let download_file = camino_tempfile::NamedUtf8TempFile::new()
        .wrap_err("failed to create temp file for ERTS download")?;
    let download_hash = download(&url, download_file.path())?;

    match target.os {
        Os::Windows => extract_zip(File::open(download_file.path())?, tmp_dir.path()),
        _ => extract_tar_gz(File::open(download_file.path())?, tmp_dir.path()),
    }?;

    let extracted = tmp_dir.keep();
    if let Err(rename_err) = fs::rename(&extracted, &cache_dir) {
        if validate(&cache_dir, target).is_ok() {
            fs::remove_dir_all(&extracted).ok();
            return Ok(ErtsResolution::Cached {
                path: cache_dir,
                otp_version,
            });
        }

        fs::remove_dir_all(&cache_dir).ok();
        if let Err(retry_err) = fs::rename(&extracted, &cache_dir) {
            fs::remove_dir_all(&extracted).ok();
            return Err(retry_err).wrap_err_with(|| {
                format!(
                    "failed to finalize ERTS cache at {cache_dir} (original error: {rename_err})"
                )
            });
        }
    }

    Ok(ErtsResolution::Downloaded {
        path: cache_dir,
        otp_version,
        url,
        hash: download_hash,
    })
}

fn cache_dir_for(otp_version: &str, target: Target) -> Result<Utf8PathBuf> {
    let proj_dirs = ProjectDirs::from("", "", "queso")
        .ok_or_else(|| eyre!("could not determine cache directory"))?;

    let cache_dir = Utf8Path::from_path(proj_dirs.cache_dir())
        .ok_or_else(|| eyre!("non-UTF-8 cache directory"))?;

    Ok(cache_dir
        .join("erts")
        .join(format!("OTP-{otp_version}"))
        .join(target.to_string()))
}

pub fn download_url(otp_version: &str, target: &Target) -> Result<String> {
    match (target.os, target.arch) {
        (Os::Macos, Arch::X86_64) => Ok(macos_url(otp_version, "amd64")),
        (Os::Macos, Arch::Aarch64) => Ok(macos_url(otp_version, "arm64")),
        (Os::Linux(libc), arch @ (Arch::X86_64 | Arch::Aarch64)) => {
            let libc_suffix = match libc {
                Libc::Glibc => "-glibc",
                Libc::Musl => "-musl",
                Libc::Static => "",
            };
            let arch_str = match arch {
                Arch::X86_64 => "x64",
                Arch::Aarch64 => "arm64",
            };
            Ok(linux_url(otp_version, arch_str, libc_suffix))
        }
        (Os::Windows, Arch::X86_64) => Ok(windows_url(otp_version)),
        _ => bail!(
            "no OTP download available for {target} (use --erts to provide a local Erlang/OTP installation)"
        ),
    }
}

fn macos_url(version: &str, arch: &str) -> String {
    format!(
        "https://github.com/erlef/otp_builds/releases/download/OTP-{version}/OTP-{version}-macos-{arch}.tar.gz"
    )
}

fn linux_url(version: &str, arch: &str, libc_suffix: &str) -> String {
    format!(
        "https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-{version}/erlang-{version}-{arch}{libc_suffix}.tar.gz"
    )
}

fn windows_url(version: &str) -> String {
    format!("https://github.com/erlang/otp/releases/download/OTP-{version}/otp_win64_{version}.zip")
}

fn download(url: &str, dest: impl AsRef<Utf8Path>) -> Result<String> {
    let dest = dest.as_ref();
    let response = ureq::get(url)
        .call()
        .wrap_err_with(|| format!("failed to download {url}"))?;

    let status = response.status();
    ensure!(status == 200, "download failed with HTTP {status}: {url}");

    let mut response_reader = response.into_body().into_reader();
    let file = File::create(dest).wrap_err_with(|| format!("failed to create {dest}"))?;
    let mut file_writer = HashingWriter::new(file);

    io::copy(&mut response_reader, &mut file_writer).wrap_err("failed to download ERTS archive")?;

    Ok(file_writer.finalize())
}

fn extract_tar_gz(reader: impl Read, dest: impl AsRef<Utf8Path>) -> Result<()> {
    let dest = dest.as_ref();
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();

        let normalized: PathBuf = path
            .components()
            .filter(|c| matches!(c, path::Component::Normal(_)))
            .collect();
        if normalized.as_os_str().is_empty() {
            continue;
        }

        let dest_path = dest.as_std_path().join(&normalized);
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if entry.header().entry_type().is_file() {
            let mut file = File::create(&dest_path)?;
            io::copy(&mut entry, &mut file)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(mode) = entry.header().mode() {
                    fs::set_permissions(&dest_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

fn extract_zip(file: File, dest: impl AsRef<Utf8Path>) -> Result<()> {
    let dest = dest.as_ref();
    let mut archive = zip::ZipArchive::new(file).wrap_err("failed to open zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(path) = file.enclosed_name() else {
            continue;
        };

        let dest_path = dest.as_std_path().join(&path);

        if file.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&dest_path)?;
            io::copy(&mut file, &mut out)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use camino_tempfile_ext::prelude::*;

    use super::*;
    use crate::target::{Arch, Os};

    const LINUX_TARGET: Target = Target {
        os: Os::Linux(Libc::Static),
        arch: Arch::X86_64,
    };

    const WINDOWS_TARGET: Target = Target {
        os: Os::Windows,
        arch: Arch::X86_64,
    };

    fn create_mock_erts(root: &Utf8TempDir, erl_suffix: &str) {
        root.child(format!("erts-16.3/bin/erl{erl_suffix}"))
            .write_str("")
            .unwrap();
        root.child("lib/kernel-10.6.1").create_dir_all().unwrap();
        root.child("lib/stdlib-7.3").create_dir_all().unwrap();
    }

    #[test]
    fn test_valid_erts() {
        let dir = camino_tempfile::tempdir().unwrap();
        create_mock_erts(&dir, "");

        let erts = validate(dir.path(), &LINUX_TARGET).unwrap();
        assert_eq!(erts.version, "16.3");
        assert!(erts.erts_dir.ends_with("erts-16.3"));
    }

    #[test]
    fn test_valid_windows_erts() {
        let dir = camino_tempfile::tempdir().unwrap();
        create_mock_erts(&dir, ".exe");

        let erts = validate(dir.path(), &WINDOWS_TARGET).unwrap();
        assert_eq!(erts.version, "16.3");
    }

    #[test]
    fn test_nonexistent_path() {
        let err = validate("/nonexistent/erts/path", &LINUX_TARGET).unwrap_err();
        assert_eq!(
            err.to_string(),
            "ERTS path does not exist: /nonexistent/erts/path"
        );
    }

    #[test]
    fn test_no_erts_directory() {
        let dir = camino_tempfile::tempdir().unwrap();
        let err = validate(dir.path(), &LINUX_TARGET).unwrap_err();
        insta::with_settings!({
            filters => vec![(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]")]
        }, {
            insta::assert_snapshot!(err.to_string(), @"invalid ERTS installation at [TEMPDIR]");
        });
    }

    #[test]
    fn test_missing_erl_binary() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("erts-16.3/bin").create_dir_all().unwrap();
        dir.child("lib/kernel-10.6.1").create_dir_all().unwrap();
        dir.child("lib/stdlib-7.3").create_dir_all().unwrap();

        let err = validate(dir.path(), &LINUX_TARGET).unwrap_err();
        insta::with_settings!({
            filters => vec![(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]")]
        }, {
            insta::assert_snapshot!(err.to_string(), @"missing erl/erlexec binary in [TEMPDIR]/erts-16.3/bin");
        });
    }

    #[test]
    fn test_download_urls() {
        let cases = [
            (Os::Macos, Arch::Aarch64),
            (Os::Macos, Arch::X86_64),
            (Os::Linux(Libc::Static), Arch::X86_64),
            (Os::Linux(Libc::Static), Arch::Aarch64),
            (Os::Linux(Libc::Glibc), Arch::X86_64),
            (Os::Linux(Libc::Glibc), Arch::Aarch64),
            (Os::Linux(Libc::Musl), Arch::X86_64),
            (Os::Linux(Libc::Musl), Arch::Aarch64),
            (Os::Windows, Arch::X86_64),
        ];

        let urls: Vec<String> = cases
            .into_iter()
            .map(|(os, arch)| {
                let target = Target { os, arch };
                format!("{target}: {}", download_url("28.4.1", &target).unwrap())
            })
            .collect();

        insta::assert_snapshot!(urls.join("\n"), @"
        aarch64-macos: https://github.com/erlef/otp_builds/releases/download/OTP-28.4.1/OTP-28.4.1-macos-arm64.tar.gz
        x86_64-macos: https://github.com/erlef/otp_builds/releases/download/OTP-28.4.1/OTP-28.4.1-macos-amd64.tar.gz
        x86_64-linux-static: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-x64.tar.gz
        aarch64-linux-static: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-arm64.tar.gz
        x86_64-linux-glibc: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-x64-glibc.tar.gz
        aarch64-linux-glibc: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-arm64-glibc.tar.gz
        x86_64-linux-musl: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-x64-musl.tar.gz
        aarch64-linux-musl: https://github.com/gleam-community/erlang-linux-builds/releases/download/OTP-28.4.1/erlang-28.4.1-arm64-musl.tar.gz
        x86_64-windows: https://github.com/erlang/otp/releases/download/OTP-28.4.1/otp_win64_28.4.1.zip
        ");
    }

    #[test]
    fn test_download_url_unsupported_target() {
        let target = Target {
            os: Os::Windows,
            arch: Arch::Aarch64,
        };
        let err = download_url("28.4.1", &target).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no OTP download available for aarch64-windows (use --erts to provide a local Erlang/OTP installation)"
        );
    }

    #[test]
    fn test_aarch64_windows_has_no_download_url() {
        let target = "aarch64-windows".parse::<Target>().unwrap();
        assert!(download_url("28.4.1", &target).is_err());
    }

    #[test]
    fn test_corrupt_cache_detected() {
        let dir = camino_tempfile::tempdir().unwrap();
        dir.child("lib/kernel-10.6.1").create_dir_all().unwrap();
        assert!(validate(dir.path(), &LINUX_TARGET).is_err());
    }

    #[test]
    fn test_cache_dir_contains_version_and_target() {
        let target = Target {
            os: Os::Linux(Libc::Static),
            arch: Arch::X86_64,
        };
        let dir = cache_dir_for("28.4.1", target).unwrap();
        assert!(dir.ends_with("queso/erts/OTP-28.4.1/x86_64-linux-static"));
    }
}
