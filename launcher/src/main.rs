mod format;

use std::{
    env,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    process::{self, Command},
};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use directories::ProjectDirs;
use eyre::{OptionExt, Result, eyre};
use format::{Metadata, TRAILER_SIZE, Trailer};

const EXTRACT_TMP_PREFIX: &str = ".tmp-extract-";

fn main() {
    if let Err(err) = run() {
        eprintln!("queso: {err:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let exe_path = env::current_exe()?;
    let mut file = File::open(&exe_path)?;
    let file_len = file.metadata()?.len();
    let trailer = Trailer::read(&mut file)?;
    trailer.validate(file_len)?;
    let metadata = read_metadata(&mut file, &trailer)?;

    let base_dir = cache_dir(&metadata.name)?;
    let (erts_dir, app_dir) = cache_paths(&base_dir, &metadata);

    if !erts_dir.is_dir() {
        extract(
            &mut file,
            trailer.erts_offset,
            trailer.app_offset - trailer.erts_offset,
            &erts_dir,
        )?;
    }

    if !app_dir.is_dir() {
        extract(
            &mut file,
            trailer.app_offset,
            trailer.meta_offset - trailer.app_offset,
            &app_dir,
        )?;
    }

    if let Some(name) = erts_dir.file_name() {
        clean_stale_cache(&base_dir.join("erts"), name);
    }
    if let Some(name) = app_dir.file_name() {
        clean_stale_cache(&base_dir.join("app"), name);
    }

    boot_erts(&erts_dir, &app_dir, &metadata)
}

fn clean_stale_cache(dir: &Utf8Path, current: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name == current || name.starts_with(EXTRACT_TMP_PREFIX) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

fn read_metadata(file: &mut File, trailer: &Trailer) -> Result<Metadata> {
    let trailer_size = i64::try_from(TRAILER_SIZE)?;
    let meta_end = file.seek(SeekFrom::End(-trailer_size))?;
    let meta_len = usize::try_from(
        meta_end
            .checked_sub(trailer.meta_offset)
            .ok_or_eyre("meta_offset past end of metadata region")?,
    )?;

    file.seek(SeekFrom::Start(trailer.meta_offset))?;
    let mut meta_bytes = vec![0u8; meta_len];
    file.read_exact(&mut meta_bytes)?;

    let (metadata, _): (Metadata, _) =
        bincode::decode_from_slice(&meta_bytes, bincode::config::standard())?;
    metadata.validate()?;
    Ok(metadata)
}

fn cache_dir(name: &str) -> Result<Utf8PathBuf> {
    let proj_dirs =
        ProjectDirs::from("", "", name).ok_or_eyre("could not determine cache directory")?;
    let base =
        Utf8Path::from_path(proj_dirs.cache_dir()).ok_or_eyre("non-UTF-8 cache directory")?;
    Ok(base.to_path_buf())
}

fn cache_paths(base_dir: &Utf8Path, metadata: &Metadata) -> (Utf8PathBuf, Utf8PathBuf) {
    let erts_fp_len = metadata.erts_hash.len().min(12);
    let app_fp_len = metadata.app_hash.len().min(12);
    let erts_dir = base_dir
        .join("erts")
        .join(&metadata.erts_hash[..erts_fp_len]);
    let app_dir = base_dir.join("app").join(format!(
        "{}-{}",
        metadata.version,
        &metadata.app_hash[..app_fp_len],
    ));
    (erts_dir, app_dir)
}

fn extract(file: &mut File, offset: u64, length: u64, install_dir: &Utf8Path) -> Result<()> {
    let parent = install_dir
        .parent()
        .ok_or_eyre("install dir has no parent")?;
    fs::create_dir_all(parent)?;

    let tmp_dir = camino_tempfile::Builder::new()
        .prefix(EXTRACT_TMP_PREFIX)
        .tempdir_in(parent)?;
    let dest = tmp_dir.path();
    file.seek(SeekFrom::Start(offset))?;
    let bounded = file.take(length);
    let decoder = zstd::Decoder::new(bounded)?;
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = Utf8PathBuf::from_path_buf(entry.path()?.into_owned())
            .map_err(|p| eyre!("non-UTF-8 path in archive: {}", p.display()))?;

        let normalized: Utf8PathBuf = path
            .components()
            .filter(|c| matches!(c, Utf8Component::Normal(_)))
            .collect();
        if normalized.as_str().is_empty() {
            continue;
        }

        let dest_path = dest.join(&normalized);
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if entry.header().entry_type().is_file() {
            let mode = entry.header().mode().ok();
            extract_file(&mut entry, &dest_path, mode)?;
        }
    }

    let dir = tmp_dir.keep();
    match fs::rename(&dir, install_dir) {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
            ) && install_dir.is_dir() =>
        {
            fs::remove_dir_all(&dir).ok();
            Ok(())
        }
        Err(err) => {
            fs::remove_dir_all(&dir).ok();
            Err(err.into())
        }
    }
}

fn extract_file(reader: &mut impl Read, dest_path: &Utf8Path, mode: Option<u32>) -> Result<()> {
    let mut out = File::create(dest_path)?;
    io::copy(reader, &mut out)?;

    if let Some(mode) = mode {
        set_permissions(dest_path, mode)?;
    }

    Ok(())
}

#[cfg(unix)]
fn set_permissions(dest_path: &Utf8Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dest_path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_permissions(_dest_path: &Utf8Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn add_lib_paths(app_dir: &Utf8Path) -> Vec<String> {
    let lib_dir = app_dir.join("lib");
    let Ok(entries) = lib_dir.read_dir_utf8() else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|e| e.file_name().to_string())
        .collect();
    names.sort();

    names
        .iter()
        .flat_map(|name| {
            let ebin_path = lib_dir.join(name).join("ebin");
            ebin_path
                .is_dir()
                .then(|| ["-pa".to_string(), ebin_path.into_string()])
                .into_iter()
                .flatten()
        })
        .collect()
}

fn boot_erts(erts_dir: &Utf8Path, app_dir: &Utf8Path, metadata: &Metadata) -> Result<()> {
    let erts_bin_dir = erts_dir
        .join(format!("erts-{}", metadata.erts_version))
        .join("bin");

    let erl_name = if cfg!(target_os = "windows") {
        "erl.exe"
    } else {
        "erlexec"
    };
    let erl = erts_bin_dir.join(erl_name);

    let boot_file = erts_dir.join(&metadata.boot_path);
    let eval_arg = format!("'{}':main()", metadata.entry_module);

    let mut args: Vec<String> = vec!["-boot".to_string(), boot_file.into_string()];
    args.extend(add_lib_paths(app_dir));
    args.extend([
        "-noshell".to_string(),
        "-eval".to_string(),
        eval_arg,
        "-s".to_string(),
        "erlang".to_string(),
        "halt".to_string(),
    ]);

    let extra_args: Vec<String> = env::args().skip(1).collect();
    if !extra_args.is_empty() {
        args.push("-extra".to_string());
        args.extend(extra_args);
    }

    let mut cmd = Command::new(erl.as_str());
    cmd.args(&args)
        .env("ROOTDIR", erts_dir)
        .env("BINDIR", &erts_bin_dir)
        .env("EMU", "beam")
        .env("PROGNAME", "erl");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        Err(err.into())
    }

    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use byteorder::{LittleEndian, WriteBytesExt};

    use super::*;
    use crate::format::TRAILER_MAGIC;

    fn sample_metadata() -> Metadata {
        Metadata {
            name: "my_app".into(),
            version: "1.2.3".into(),
            entry_module: "my_app@cli".into(),
            erts_version: "15.0".into(),
            erts_hash: "abcdefabcdef0123456789".into(),
            app_hash: "deadbeefdeadbeef0011223344".into(),
            boot_path: "releases/28/no_dot_erlang".into(),
        }
    }

    #[test]
    fn test_add_lib_paths_with_ebin() {
        let dir = camino_tempfile::tempdir().unwrap();
        let app_dir = dir.path();

        fs::create_dir_all(app_dir.join("lib/gleam_stdlib/ebin")).unwrap();
        fs::create_dir_all(app_dir.join("lib/my_app/ebin")).unwrap();
        fs::create_dir_all(app_dir.join("lib/no_ebin")).unwrap();

        let paths = add_lib_paths(app_dir);

        assert_eq!(paths.len(), 4);
        assert_eq!(paths[0], "-pa");
        assert!(paths[1].ends_with("gleam_stdlib/ebin"));
        assert_eq!(paths[2], "-pa");
        assert!(paths[3].ends_with("my_app/ebin"));
    }

    #[test]
    fn test_add_lib_paths_no_lib_dir() {
        let dir = camino_tempfile::tempdir().unwrap();
        let paths = add_lib_paths(dir.path());
        assert!(paths.is_empty());
    }

    fn write_test_binary(path: &Utf8Path, erts: &[u8], app: &[u8], meta: &Metadata) -> Trailer {
        let meta_bytes = bincode::encode_to_vec(meta, bincode::config::standard()).unwrap();
        let erts_offset = 0u64;
        let app_offset = erts_offset + u64::try_from(erts.len()).unwrap();
        let meta_offset = app_offset + u64::try_from(app.len()).unwrap();

        let mut f = File::create(path).unwrap();
        f.write_all(erts).unwrap();
        f.write_all(app).unwrap();
        f.write_all(&meta_bytes).unwrap();
        f.write_u64::<LittleEndian>(erts_offset).unwrap();
        f.write_u64::<LittleEndian>(app_offset).unwrap();
        f.write_u64::<LittleEndian>(meta_offset).unwrap();
        f.write_all(TRAILER_MAGIC).unwrap();

        Trailer {
            erts_offset,
            app_offset,
            meta_offset,
        }
    }

    #[test]
    fn test_read_metadata_happy_path() {
        let dir = camino_tempfile::tempdir().unwrap();
        let path = dir.path().join("binary");
        let meta = sample_metadata();
        let trailer = write_test_binary(&path, b"ERTS", b"APP", &meta);

        let mut f = File::open(&path).unwrap();
        let decoded = read_metadata(&mut f, &trailer).unwrap();
        assert_eq!(decoded.name, meta.name);
        assert_eq!(decoded.entry_module, meta.entry_module);
        assert_eq!(decoded.boot_path, meta.boot_path);
    }

    #[test]
    fn test_clean_stale_cache_removes_non_matching_siblings() {
        let dir = camino_tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("current")).unwrap();
        fs::create_dir_all(dir.path().join("stale1")).unwrap();
        fs::create_dir_all(dir.path().join("stale2")).unwrap();
        fs::write(dir.path().join("stale1/file"), b"x").unwrap();

        clean_stale_cache(dir.path(), "current");

        assert!(dir.path().join("current").is_dir());
        assert!(!dir.path().join("stale1").exists());
        assert!(!dir.path().join("stale2").exists());
    }

    #[test]
    fn test_clean_stale_cache_preserves_in_flight_tmp_dirs() {
        let dir = camino_tempfile::tempdir().unwrap();
        let in_flight = format!("{EXTRACT_TMP_PREFIX}abc123");
        fs::create_dir_all(dir.path().join("current")).unwrap();
        fs::create_dir_all(dir.path().join(&in_flight)).unwrap();
        fs::create_dir_all(dir.path().join("stale")).unwrap();

        clean_stale_cache(dir.path(), "current");

        assert!(dir.path().join("current").is_dir());
        assert!(dir.path().join(&in_flight).is_dir());
        assert!(!dir.path().join("stale").exists());
    }

    #[test]
    fn test_clean_stale_cache_missing_dir_is_noop() {
        let dir = camino_tempfile::tempdir().unwrap();
        clean_stale_cache(&dir.path().join("nonexistent"), "current");
    }

    #[test]
    fn test_read_metadata_rejects_meta_offset_past_end() {
        let dir = camino_tempfile::tempdir().unwrap();
        let path = dir.path().join("binary");
        let trailer = write_test_binary(&path, b"ERTS", b"APP", &sample_metadata());
        let bad_trailer = Trailer {
            erts_offset: trailer.erts_offset,
            app_offset: trailer.app_offset,
            meta_offset: trailer.meta_offset + 1_000_000,
        };

        let mut f = File::open(&path).unwrap();
        assert!(read_metadata(&mut f, &bad_trailer).is_err());
    }

    fn build_tar_zstd(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(data.len()).unwrap());
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);

            let bytes = header.as_mut_bytes();
            let name = path.as_bytes();
            bytes[..name.len()].copy_from_slice(name);

            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        zstd::stream::encode_all(&tar_bytes[..], 0).unwrap()
    }

    #[test]
    fn test_extract_happy_path() {
        let tmp = camino_tempfile::tempdir().unwrap();
        let archive_path = tmp.path().join("archive.bin");
        let archive = build_tar_zstd(&[("hello.txt", b"hi")]);
        fs::write(&archive_path, &archive).unwrap();

        let install_dir = tmp.path().join("install");
        let mut f = File::open(&archive_path).unwrap();
        let len = u64::try_from(archive.len()).unwrap();
        extract(&mut f, 0, len, &install_dir).unwrap();

        let content = fs::read_to_string(install_dir.join("hello.txt")).unwrap();
        assert_eq!(content, "hi");
    }

    #[test]
    fn test_extract_strips_parent_traversal() {
        let tmp = camino_tempfile::tempdir().unwrap();
        let archive_path = tmp.path().join("archive.bin");
        let archive = build_tar_zstd(&[("../escape.txt", b"pwn")]);
        fs::write(&archive_path, &archive).unwrap();

        let install_dir = tmp.path().join("install");
        let mut f = File::open(&archive_path).unwrap();
        let len = u64::try_from(archive.len()).unwrap();
        extract(&mut f, 0, len, &install_dir).unwrap();

        assert!(!tmp.path().join("escape.txt").exists());
        let content = fs::read_to_string(install_dir.join("escape.txt")).unwrap();
        assert_eq!(content, "pwn");
    }
}
