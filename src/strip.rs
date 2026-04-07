use std::collections::HashMap;

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Result, eyre};
use walkdir::WalkDir;

use crate::erl::Erl;

fn collect_beam_files(dir: impl AsRef<Utf8Path>) -> Result<Vec<Utf8PathBuf>> {
    let dir = dir.as_ref();
    let mut paths = Vec::new();
    for entry in WalkDir::new(dir).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = Utf8Path::from_path(entry.path())
            .ok_or_else(|| eyre!("non-UTF-8 path: {}", entry.path().display()))?;
        if path.extension() == Some("beam") {
            paths.push(path.to_path_buf());
        }
    }
    Ok(paths)
}

pub fn strip_directory(
    erl: &mut Erl,
    dir: impl AsRef<Utf8Path>,
) -> Result<HashMap<Utf8PathBuf, Vec<u8>>> {
    let paths = collect_beam_files(dir)?;
    erl.strip_beam_files(&paths)
}

#[cfg(test)]
mod test {
    use std::fs;

    use super::*;

    #[test]
    fn test_empty_input() {
        let mut erl = Erl::spawn().unwrap();
        let result = erl.strip_beam_files(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_strip_reduces_size() {
        let paths: Vec<Utf8PathBuf> = ["simple.beam", "documented.beam", "with_literals.beam"]
            .iter()
            .map(|name| Utf8PathBuf::from(format!("tests/fixtures/beam/{name}")))
            .collect();
        let originals: Vec<usize> = paths.iter().map(|p| fs::read(p).unwrap().len()).collect();

        let mut erl = Erl::spawn().unwrap();
        let result = erl.strip_beam_files(&paths).unwrap();

        assert_eq!(result.len(), 3);
        for (path, original_len) in paths.iter().zip(originals.iter()) {
            let stripped = result.get(path).unwrap();
            assert!(stripped.len() < *original_len);
        }
    }
}
