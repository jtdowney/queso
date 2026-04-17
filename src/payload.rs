use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Write,
};

use camino::{Utf8Path, Utf8PathBuf};
use directories::ProjectDirs;
use eyre::{Context, Result, eyre};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{
    HashingWriter,
    erl::Erl,
    erts::Erts,
    strip,
    target::{Os, Target},
};

const STRIPPED_DIRS: &[&str] = &["src", "doc", "examples", "include", "c_src"];
const KEPT_ERTS_BINARIES: &[&str] = &[
    "beam.smp",
    "erl",
    "erl_child_setup",
    "erlexec",
    "inet_gethost",
];

pub fn assemble_erts(
    erl: &mut Erl,
    output_path: impl AsRef<Utf8Path>,
    erts: &Erts,
    allowed_otp_apps: Option<&HashSet<String>>,
    strip_beam: bool,
    target: &Target,
    compression_level: i32,
) -> Result<String> {
    let output_path = output_path.as_ref();
    let hash = erts_config_hash(&erts.version, *target, allowed_otp_apps, strip_beam);

    if let Some(cached) = find_cached_erts(&hash, *target, compression_level) {
        fs::copy(&cached, output_path)
            .wrap_err_with(|| format!("failed to copy cached ERTS from {cached}"))?;
        return Ok(hash);
    }

    let stripped = maybe_strip(erl, &erts.root, strip_beam)?;
    let file =
        File::create(output_path).wrap_err_with(|| format!("failed to create {output_path}"))?;
    let mut enc = zstd::Encoder::new(file, compression_level)?;
    let mut builder = tar::Builder::new(&mut enc);
    add_erts_directory(
        &mut builder,
        &erts.root,
        allowed_otp_apps,
        &stripped,
        *target,
    )?;
    builder.into_inner()?;
    enc.finish()?;

    if let Some(cache_path) = erts_cache_path(&hash, *target, compression_level) {
        save_cached_erts(&cache_path, output_path);
    }

    Ok(hash)
}

pub fn assemble_app(
    erl: &mut Erl,
    output_path: impl AsRef<Utf8Path>,
    erlang_build_dir: impl AsRef<Utf8Path>,
    strip_beam: bool,
    compression_level: i32,
) -> Result<String> {
    let output_path = output_path.as_ref();

    let stripped = maybe_strip(erl, erlang_build_dir.as_ref(), strip_beam)?;
    let file =
        File::create(output_path).wrap_err_with(|| format!("failed to create {output_path}"))?;
    let hashing_file = HashingWriter::new(file);
    let mut enc = zstd::Encoder::new(hashing_file, compression_level)?;

    let mut builder = tar::Builder::new(&mut enc);
    add_directory(&mut builder, erlang_build_dir.as_ref(), "lib", &stripped)?;
    builder.into_inner()?;

    let hashing_file = enc.finish()?;
    let hash = hashing_file.finalize();

    Ok(hash)
}

fn maybe_strip(
    erl: &mut Erl,
    dir: &Utf8Path,
    strip_beam: bool,
) -> Result<HashMap<Utf8PathBuf, Vec<u8>>> {
    if strip_beam {
        strip::strip_directory(erl, dir)
    } else {
        Ok(HashMap::new())
    }
}

fn erts_config_hash(
    erts_version: &str,
    target: Target,
    allowed_otp_apps: Option<&HashSet<String>>,
    strip_beam: bool,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(erts_version.as_bytes());
    hasher.update(target.to_string().as_bytes());
    hasher.update([u8::from(strip_beam)]);
    if let Some(apps) = allowed_otp_apps {
        let mut sorted: Vec<_> = apps.iter().collect();
        sorted.sort();
        for app in sorted {
            hasher.update(app.as_bytes());
        }
    } else {
        hasher.update(b"full_erts");
    }
    hex::encode(hasher.finalize())
}

fn erts_cache_path(hash: &str, target: Target, compression_level: i32) -> Option<Utf8PathBuf> {
    let dirs = ProjectDirs::from("", "", "queso")?;
    let cache_dir = Utf8PathBuf::from_path_buf(dirs.cache_dir().join("payload")).ok()?;
    Some(
        cache_dir
            .join(target.to_string())
            .join(format!("erts_{hash}_zstd{compression_level}.tar.zst")),
    )
}

fn find_cached_erts(hash: &str, target: Target, compression_level: i32) -> Option<Utf8PathBuf> {
    let path = erts_cache_path(hash, target, compression_level)?;
    path.is_file().then_some(path)
}

fn save_cached_erts(dest: &Utf8Path, src: &Utf8Path) {
    let Some(dir) = dest.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }

    let Ok(tmp) = camino_tempfile::Builder::new()
        .prefix(".erts_cache_")
        .tempfile_in(dir)
    else {
        return;
    };

    if fs::copy(src, tmp.path()).is_err() {
        return;
    }

    tmp.persist(dest).ok();
}

fn file_mode(path: impl AsRef<Utf8Path>) -> u32 {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).map_or(0o644, |m| m.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o644
    }
}

fn append_data<W: Write>(
    builder: &mut tar::Builder<W>,
    path: impl AsRef<Utf8Path>,
    data: &[u8],
    mode: u32,
) -> Result<()> {
    let path = path.as_ref();
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, path, data)?;
    Ok(())
}

fn read_or_use_stripped(
    path: impl AsRef<Utf8Path>,
    stripped: &HashMap<Utf8PathBuf, Vec<u8>>,
) -> Result<Vec<u8>> {
    let path = path.as_ref();
    if let Some(data) = stripped.get(path) {
        return Ok(data.clone());
    }
    Ok(std::fs::read(path)?)
}

fn add_directory<W: Write>(
    builder: &mut tar::Builder<W>,
    src: impl AsRef<Utf8Path>,
    prefix: impl AsRef<Utf8Path>,
    stripped: &HashMap<Utf8PathBuf, Vec<u8>>,
) -> Result<()> {
    let src = src.as_ref();
    let prefix = prefix.as_ref();
    for entry in WalkDir::new(src).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = Utf8Path::from_path(entry.path())
            .ok_or_else(|| eyre!("non-UTF-8 path: {}", entry.path().display()))?;
        let relative = path.strip_prefix(src)?;
        let archive_path = prefix.join(relative);
        let data = read_or_use_stripped(path, stripped)?;
        let mode = file_mode(path);

        append_data(builder, &archive_path, &data, mode)?;
    }
    Ok(())
}

fn kept_erts_binary_names(target: Target) -> HashSet<String> {
    let suffix = target.exe_suffix();
    let mut names: HashSet<String> = KEPT_ERTS_BINARIES
        .iter()
        .map(|b| format!("{b}{suffix}"))
        .collect();

    if target.os == Os::Windows {
        names.extend(KEPT_ERTS_BINARIES.iter().map(|b| format!("{b}.dll")));
    }

    names
}

fn should_include_erts_file(
    relative: &Utf8Path,
    allowed: &HashSet<String>,
    kept_binaries: &HashSet<String>,
) -> bool {
    let parts: Vec<_> = relative.components().map(|c| c.as_str()).collect();
    match *parts.as_slice() {
        ["lib", _, dir, ..] if STRIPPED_DIRS.contains(&dir) => false,
        ["lib", app_dir, ..] => {
            let app_name = app_dir.split('-').next().unwrap_or(app_dir);
            allowed.contains(app_name)
        }
        [erts, "bin", binary] if erts.starts_with("erts-") => kept_binaries.contains(binary),
        [erts, "ebin", ..] if erts.starts_with("erts-") => true,
        ["releases", _, "no_dot_erlang.boot"] => true,
        _ => false,
    }
}

fn is_erts_binary(relative: impl AsRef<Utf8Path>) -> bool {
    let parts: Vec<_> = relative.as_ref().components().map(|c| c.as_str()).collect();
    match parts.as_slice() {
        [erts, "bin", _] if erts.starts_with("erts-") => true,
        ["bin", _] => true,
        _ => false,
    }
}

fn add_erts_directory<W: Write>(
    builder: &mut tar::Builder<W>,
    src: impl AsRef<Utf8Path>,
    allowed_otp_apps: Option<&HashSet<String>>,
    stripped: &HashMap<Utf8PathBuf, Vec<u8>>,
    target: Target,
) -> Result<()> {
    let src = src.as_ref();
    let kept_binaries = kept_erts_binary_names(target);

    for entry in WalkDir::new(src).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = Utf8Path::from_path(entry.path())
            .ok_or_else(|| eyre!("non-UTF-8 path: {}", entry.path().display()))?;
        let relative = path.strip_prefix(src)?;

        if let Some(allowed) = allowed_otp_apps
            && !should_include_erts_file(relative, allowed, &kept_binaries)
        {
            continue;
        }

        let data = read_or_use_stripped(path, stripped)?;
        let mode = if is_erts_binary(relative) {
            0o755
        } else {
            file_mode(path)
        };
        append_data(builder, relative, &data, mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use camino_tempfile_ext::{fixture::ChildPath, prelude::*};

    use super::*;
    use crate::target::{Arch, Libc, Target};

    fn create_mock_erts(root: &Utf8TempDir) -> (Erts, ChildPath) {
        let erts_dir = root.child("erts-16.3");
        erts_dir.child("bin/erl").write_str("#!/bin/sh\n").unwrap();
        root.child("lib/kernel-10.6.1").create_dir_all().unwrap();
        root.child("lib/stdlib-7.3").create_dir_all().unwrap();

        let erts = Erts {
            root: root.path().to_path_buf(),
            version: "16.3".into(),
            erts_dir: erts_dir.to_path_buf(),
        };

        (erts, erts_dir)
    }

    fn create_mock_gleam_build(dir: &Utf8TempDir) -> Utf8PathBuf {
        let shipment = dir.child("build/erlang-shipment");
        shipment
            .child("my_app/ebin/my_app.beam")
            .write_binary(b"BEAM_MOCK")
            .unwrap();
        shipment.to_path_buf()
    }

    fn read_tar_entries(data: &[u8]) -> Vec<String> {
        let mut archive = tar::Archive::new(data);
        let mut entries: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        entries
    }

    const LINUX_TARGET: Target = Target {
        os: Os::Linux(Libc::Musl),
        arch: Arch::X86_64,
    };

    fn build_and_read_erts_entries(
        erl: &mut Erl,
        erts: &Erts,
        allowed: Option<&HashSet<String>>,
    ) -> Vec<String> {
        let stripped = maybe_strip(erl, &erts.root, true).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_erts_directory(&mut builder, &erts.root, allowed, &stripped, LINUX_TARGET).unwrap();
        read_tar_entries(&builder.into_inner().unwrap())
    }

    fn build_and_read_app_entries(erl: &mut Erl, erlang_dir: &Utf8Path) -> Vec<String> {
        let stripped = maybe_strip(erl, erlang_dir, true).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_directory(&mut builder, erlang_dir, "lib", &stripped).unwrap();
        read_tar_entries(&builder.into_inner().unwrap())
    }

    #[test]
    fn test_erts_payload_filters_lib() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, _) = create_mock_erts(&root);

        root.child("lib/snmp-5.20.1/ebin/snmp.beam")
            .write_binary(b"BEAM")
            .unwrap();
        root.child("lib/kernel-10.6.1/ebin/kernel.beam")
            .write_binary(b"BEAM")
            .unwrap();

        let allowed: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let entries = build_and_read_erts_entries(&mut erl, &erts, Some(&allowed));
        insta::assert_yaml_snapshot!(entries, @"
        - erts-16.3/bin/erl
        - lib/kernel-10.6.1/ebin/kernel.beam
        ");
    }

    #[test]
    fn test_erts_payload_strips_non_essential_dirs() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, _) = create_mock_erts(&root);

        let kernel_dir = root.child("lib/kernel-10.6.1");
        kernel_dir
            .child("ebin/kernel.beam")
            .write_binary(b"BEAM")
            .unwrap();
        kernel_dir
            .child("src/kernel.erl")
            .write_binary(b"source")
            .unwrap();
        kernel_dir
            .child("doc/index.html")
            .write_binary(b"docs")
            .unwrap();
        kernel_dir
            .child("include/file.hrl")
            .write_binary(b"header")
            .unwrap();

        let allowed: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let entries = build_and_read_erts_entries(&mut erl, &erts, Some(&allowed));
        insta::assert_yaml_snapshot!(entries, @"
        - erts-16.3/bin/erl
        - lib/kernel-10.6.1/ebin/kernel.beam
        ");
    }

    #[test]
    fn test_erts_payload_strips_core_to_bin_only() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, erts_dir) = create_mock_erts(&root);

        erts_dir
            .child("include/erl_nif.h")
            .write_binary(b"header")
            .unwrap();
        erts_dir
            .child("erl_driver.h")
            .write_binary(b"header")
            .unwrap();
        erts_dir.child("src/foo.c").write_binary(b"source").unwrap();
        erts_dir
            .child("lib/internal.a")
            .write_binary(b"archive")
            .unwrap();
        erts_dir.child("man/erl.1").write_binary(b"man").unwrap();
        erts_dir
            .child("ebin/init.beam")
            .write_binary(b"BEAM")
            .unwrap();

        let allowed: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let entries = build_and_read_erts_entries(&mut erl, &erts, Some(&allowed));
        insta::assert_yaml_snapshot!(entries, @"
        - erts-16.3/bin/erl
        - erts-16.3/ebin/init.beam
        ");
    }

    #[test]
    fn test_erts_payload_strips_unnecessary_binaries() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, erts_dir) = create_mock_erts(&root);

        let bin_dir = erts_dir.child("bin");
        bin_dir.child("beam.smp").write_binary(b"beam").unwrap();
        bin_dir.child("erlexec").write_binary(b"erlexec").unwrap();
        bin_dir
            .child("erl_child_setup")
            .write_binary(b"child")
            .unwrap();
        bin_dir.child("inet_gethost").write_binary(b"inet").unwrap();
        bin_dir.child("epmd").write_binary(b"epmd").unwrap();
        bin_dir.child("heart").write_binary(b"heart").unwrap();
        bin_dir.child("erlc").write_binary(b"erlc").unwrap();
        bin_dir.child("ct_run").write_binary(b"ct_run").unwrap();
        bin_dir.child("dialyzer").write_binary(b"dialyzer").unwrap();
        bin_dir.child("typer").write_binary(b"typer").unwrap();

        let allowed: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let entries = build_and_read_erts_entries(&mut erl, &erts, Some(&allowed));
        insta::assert_yaml_snapshot!(entries, @"
        - erts-16.3/bin/beam.smp
        - erts-16.3/bin/erl
        - erts-16.3/bin/erl_child_setup
        - erts-16.3/bin/erlexec
        - erts-16.3/bin/inet_gethost
        ");
    }

    #[test]
    fn test_erts_payload_strips_non_runtime_top_level_dirs() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, _) = create_mock_erts(&root);

        root.child("bin/erlc").write_binary(b"erlc").unwrap();
        root.child("bin/escript").write_binary(b"escript").unwrap();
        root.child("bin/erl").write_binary(b"erl").unwrap();
        root.child("misc/format_man_pages")
            .write_binary(b"misc")
            .unwrap();
        root.child("usr/include/erl_nif.h")
            .write_binary(b"header")
            .unwrap();
        root.child("usr/lib/libei.a")
            .write_binary(b"archive")
            .unwrap();
        root.child("releases/28/no_dot_erlang.boot")
            .write_binary(b"boot")
            .unwrap();
        root.child("releases/28/start_clean.boot")
            .write_binary(b"boot")
            .unwrap();
        root.child("releases/28/start.script")
            .write_binary(b"script")
            .unwrap();
        root.child("releases/RELEASES")
            .write_binary(b"releases")
            .unwrap();
        root.child("lib/kernel-10.6.1/ebin/kernel.beam")
            .write_binary(b"BEAM")
            .unwrap();

        let allowed: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();

        let entries = build_and_read_erts_entries(&mut erl, &erts, Some(&allowed));
        insta::assert_yaml_snapshot!(entries, @"
        - erts-16.3/bin/erl
        - lib/kernel-10.6.1/ebin/kernel.beam
        - releases/28/no_dot_erlang.boot
        ");
    }

    #[test]
    fn test_is_erts_binary() {
        let cases: &[(&str, bool)] = &[
            ("erts-16.3/bin/erlexec", true),
            ("erts-16.3/bin/beam.smp", true),
            ("erts-16.3/bin/erlc", true),
            ("bin/erl", true),
            ("erts-16.3/bin/erl.exe", true),
            ("erts-16.3/bin/beam.smp.dll", true),
            ("erts-16.3/bin/erlexec.dll", true),
            ("erts-16.3/bin/erlc.exe", true),
            ("erts-16.3/ebin/init.beam", false),
            ("lib/kernel-10.6.1/ebin/kernel.beam", false),
        ];

        for (path, expected) in cases {
            assert_eq!(is_erts_binary(path), *expected);
        }
    }

    #[test]
    fn test_erts_payload_contains_expected_structure() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let (erts, _) = create_mock_erts(&root);

        let entries = build_and_read_erts_entries(&mut erl, &erts, None);
        insta::assert_yaml_snapshot!(entries, @"- erts-16.3/bin/erl");
    }

    #[test]
    fn test_app_payload_contains_expected_structure() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let erlang_dir = create_mock_gleam_build(&root);

        let entries = build_and_read_app_entries(&mut erl, &erlang_dir);
        insta::assert_yaml_snapshot!(entries, @"- lib/my_app/ebin/my_app.beam");
    }

    #[test]
    fn test_erts_hash_sensitivity() {
        let target = "aarch64-macos".parse::<Target>().unwrap();
        let other_target = "x86_64-linux-static".parse::<Target>().unwrap();
        let apps: HashSet<String> = ["kernel", "stdlib"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let other_apps: HashSet<String> = ["kernel", "stdlib", "crypto"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let baseline = erts_config_hash("15.0", target, Some(&apps), true);

        assert_eq!(
            baseline,
            erts_config_hash("15.0", target, Some(&apps), true)
        );
        assert_ne!(
            baseline,
            erts_config_hash("16.0", target, Some(&apps), true)
        );
        assert_ne!(
            baseline,
            erts_config_hash("15.0", other_target, Some(&apps), true)
        );
        assert_ne!(
            baseline,
            erts_config_hash("15.0", target, Some(&other_apps), true)
        );
        assert_ne!(
            baseline,
            erts_config_hash("15.0", target, Some(&apps), false)
        );
        assert_ne!(baseline, erts_config_hash("15.0", target, None, true));
    }

    fn build_app_tar_bytes(erl: &mut Erl, erlang_dir: &Utf8Path) -> Vec<u8> {
        let stripped = maybe_strip(erl, erlang_dir, true).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_directory(&mut builder, erlang_dir, "lib", &stripped).unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn test_app_hash_is_deterministic() {
        let mut erl = Erl::spawn().unwrap();
        let root = camino_tempfile::tempdir().unwrap();
        let erlang_dir = create_mock_gleam_build(&root);

        let tar1 = build_app_tar_bytes(&mut erl, &erlang_dir);
        let tar2 = build_app_tar_bytes(&mut erl, &erlang_dir);

        assert_eq!(
            hex::encode(Sha256::digest(&tar1)),
            hex::encode(Sha256::digest(&tar2))
        );
    }
}
