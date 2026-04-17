use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdout, Command, Stdio},
};

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Context, OptionExt, Result, bail, ensure, eyre};

const ESCRIPT_SOURCE: &str = include_str!("erl/sidecar.erl");

pub struct Erl {
    child: Child,
    stdout: BufReader<ChildStdout>,
    _escript_file: camino_tempfile::NamedUtf8TempFile,
}

impl Erl {
    pub fn spawn() -> Result<Self> {
        let mut escript_file = camino_tempfile::Builder::new()
            .prefix("queso-sidecar-")
            .suffix(".erl")
            .tempfile()
            .wrap_err("failed to create temp file for escript")?;
        escript_file.write_all(ESCRIPT_SOURCE.as_bytes())?;
        escript_file.flush()?;

        let mut child = Command::new("escript")
            .arg(escript_file.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .wrap_err("escript not found on PATH")?;

        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_eyre("failed to open escript stdout")?,
        );

        Ok(Self {
            child,
            stdout,
            _escript_file: escript_file,
        })
    }

    pub fn get_otp_version(&mut self) -> Result<String> {
        self.send("get_otp_version.\n")?;
        let line = self.recv_line()?;
        ensure!(
            !line.is_empty(),
            "could not determine OTP version from escript"
        );

        Ok(line)
    }

    pub fn strip_beam_files(
        &mut self,
        paths: &[Utf8PathBuf],
    ) -> Result<HashMap<Utf8PathBuf, Vec<u8>>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }

        let tmp_dir = camino_tempfile::tempdir()?;

        let file_list = paths
            .iter()
            .map(|p| {
                format!(
                    "\"{}\"",
                    p.as_str().replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        let request = format!(
            "{{strip_beam, \"{}\", [{}]}}.\n",
            tmp_dir
                .path()
                .as_str()
                .replace('\\', "\\\\")
                .replace('"', "\\\""),
            file_list
        );
        self.send(&request)?;
        let line = self.recv_line()?;

        ensure!(line == "ok", "unexpected strip_beam response: {line}");

        paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let stripped_path = tmp_dir.path().join(i.to_string());
                let data = fs::read(stripped_path)?;
                Ok((path.clone(), data))
            })
            .collect()
    }

    pub fn walk_app_dependencies(
        &mut self,
        dir: &Utf8Path,
    ) -> Result<HashMap<String, Vec<String>>> {
        let request = format!(
            "{{parse_app_files, \"{}\"}}.\n",
            dir.as_str().replace('\\', "\\\\").replace('"', "\\\"")
        );
        self.send(&request)?;
        let line = self.recv_line()?;

        if line.is_empty() {
            return Ok(HashMap::new());
        }

        line.split(';')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let (name, deps_str) = entry
                    .split_once(':')
                    .ok_or_else(|| eyre!("malformed app entry: {entry}"))?;

                let deps = if deps_str.is_empty() {
                    Vec::new()
                } else {
                    deps_str.split(',').map(String::from).collect()
                };

                Ok((name.to_string(), deps))
            })
            .collect()
    }

    fn send(&mut self, request: &str) -> Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_eyre("escript stdin is closed")?;
        stdin
            .write_all(request.as_bytes())
            .wrap_err("escript process died unexpectedly")?;
        stdin.flush().wrap_err("failed to flush to escript")?;
        Ok(())
    }

    fn recv_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .wrap_err("failed to read from escript")?;

        ensure!(bytes_read != 0, "escript process exited unexpectedly");

        let trimmed_len = line.trim_end().len();
        line.truncate(trimmed_len);

        if let Some(msg) = line.strip_prefix("ERROR: ") {
            bail!("escript error: {msg}");
        }

        Ok(line)
    }
}

impl Drop for Erl {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod test {
    use camino_tempfile_ext::prelude::*;

    use super::*;

    #[test]
    fn test_spawn_and_get_otp_version() {
        let mut erl = Erl::spawn().unwrap();
        let version = erl.get_otp_version().unwrap();
        assert!(!version.is_empty());
        assert!(version.chars().next().unwrap().is_ascii_digit());
    }

    #[test]
    fn test_reused_across_calls() {
        let mut erl = Erl::spawn().unwrap();
        let v1 = erl.get_otp_version().unwrap();
        let v2 = erl.get_otp_version().unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_walk_app_dependencies_empty_dir() {
        let mut erl = Erl::spawn().unwrap();
        let dir = camino_tempfile::tempdir().unwrap();
        let result = erl.walk_app_dependencies(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_walk_app_dependencies_with_deps() {
        let mut erl = Erl::spawn().unwrap();
        let dir = camino_tempfile::tempdir().unwrap();

        dir.child("my_app-1.0.0/ebin/my_app.app")
            .write_str(
                r#"{application, my_app, [
                {vsn, "1.0.0"},
                {applications, [kernel, stdlib]},
                {modules, []},
                {registered, []}
            ]}."#,
            )
            .unwrap();

        let result = erl.walk_app_dependencies(dir.path()).unwrap();
        assert_eq!(
            result.get("my_app").unwrap(),
            &vec!["kernel".to_string(), "stdlib".to_string()]
        );
    }
}
