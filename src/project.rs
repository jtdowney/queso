use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Context, Result, ensure};
use serde::Deserialize;

use crate::target::Target;

#[derive(Debug, Deserialize)]
struct GleamToml {
    name: String,
    version: String,
    #[serde(default)]
    tools: Option<Tools>,
}

#[derive(Debug, Deserialize)]
struct Tools {
    #[serde(default)]
    queso: Option<QuesoConfig>,
}

#[derive(Debug, Deserialize)]
pub struct QuesoConfig {
    pub entry: Option<String>,
    #[serde(default)]
    pub targets: Vec<Target>,
    pub strip_beam: Option<bool>,
    pub compression_level: Option<i32>,
    pub full_erts: Option<bool>,
}

#[derive(Debug)]
pub struct Project {
    pub name: String,
    pub version: String,
    pub root: Utf8PathBuf,
    pub queso_config: Option<QuesoConfig>,
}

#[derive(Debug, Clone)]
pub struct Entrypoint {
    pub entry_module: String,
    pub beam_file: String,
}

impl Entrypoint {
    #[must_use]
    pub fn new(module: &str) -> Self {
        let entry_module = module.replace('.', "@");
        let beam_file = format!("{entry_module}.beam");
        Self {
            entry_module,
            beam_file,
        }
    }
}

impl Project {
    pub fn load(root: impl AsRef<Utf8Path>) -> eyre::Result<Self> {
        let root = root.as_ref();
        let toml_path = root.join("gleam.toml");
        ensure!(
            toml_path.exists(),
            "not a Gleam project: gleam.toml not found in {root}"
        );

        let content = std::fs::read_to_string(&toml_path)
            .wrap_err_with(|| format!("failed to read {toml_path}"))?;

        let gleam_toml: GleamToml =
            toml::from_str(&content).wrap_err("failed to parse gleam.toml")?;

        Ok(Self {
            name: gleam_toml.name,
            version: gleam_toml.version,
            root: root.to_path_buf(),
            queso_config: gleam_toml.tools.and_then(|t| t.queso),
        })
    }

    pub fn resolve_targets(&self, cli_targets: &[Target]) -> Result<HashSet<Target>> {
        let targets = if !cli_targets.is_empty() {
            cli_targets.to_vec()
        } else if let Some(config) = &self.queso_config
            && !config.targets.is_empty()
        {
            config.targets.clone()
        } else {
            vec![Target::current()?]
        };

        Ok(HashSet::from_iter(targets))
    }

    #[must_use]
    pub fn resolve_strip_beam(&self, cli_strip_beam: Option<bool>) -> bool {
        cli_strip_beam
            .or_else(|| self.queso_config.as_ref().and_then(|c| c.strip_beam))
            .unwrap_or(true)
    }

    pub fn resolve_compression_level(&self, cli_level: Option<i32>) -> eyre::Result<i32> {
        let level = cli_level
            .or_else(|| self.queso_config.as_ref().and_then(|c| c.compression_level))
            .unwrap_or(9);

        ensure!(
            (1..=22).contains(&level),
            "compression_level must be between 1 and 22, got {level}"
        );

        Ok(level)
    }

    #[must_use]
    pub fn resolve_full_erts(&self, cli_full_erts: bool) -> bool {
        if cli_full_erts {
            return true;
        }

        self.queso_config
            .as_ref()
            .and_then(|c| c.full_erts)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn resolve_entry(&self, cli_entry: Option<&str>) -> Entrypoint {
        let module = cli_entry
            .or_else(|| self.queso_config.as_ref().and_then(|c| c.entry.as_deref()))
            .unwrap_or(&self.name);

        Entrypoint::new(module)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_load_minimal_project() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert_eq!(project.name, "my_app");
        assert_eq!(project.version, "1.0.0");
        assert!(project.queso_config.is_none());
    }

    #[test]
    fn test_load_project_with_queso_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert_eq!(project.name, "my_app");
        assert_eq!(project.version, "2.1.0");
        let entry = project
            .queso_config
            .as_ref()
            .and_then(|c| c.entry.as_deref());
        assert_eq!(entry, Some("my_app.cli"));
        let targets = &project.queso_config.as_ref().unwrap().targets;
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].to_string(), "aarch64-macos");
        assert_eq!(targets[1].to_string(), "x86_64-linux-static");
    }

    #[test]
    fn test_missing_gleam_toml() {
        let err = Project::load("/tmp/nonexistent_queso_dir").unwrap_err();
        assert_eq!(
            err.to_string(),
            "not a Gleam project: gleam.toml not found in /tmp/nonexistent_queso_dir"
        );
    }

    #[test]
    fn test_resolve_entry() {
        let minimal = Project::load("tests/fixtures/minimal").unwrap();
        let with_config = Project::load("tests/fixtures/with_config").unwrap();

        let cases = [
            (&minimal, None, "my_app"),
            (&with_config, None, "my_app@cli"),
            (&with_config, Some("custom.mod"), "custom@mod"),
        ];

        for (project, cli_entry, expected) in cases {
            assert_eq!(project.resolve_entry(cli_entry).entry_module, expected);
        }
    }

    #[test]
    fn test_resolve_targets() {
        let minimal = Project::load("tests/fixtures/minimal").unwrap();
        let with_config = Project::load("tests/fixtures/with_config").unwrap();
        let windows: Target = "x86_64-windows".parse().unwrap();

        let current_only: HashSet<Target> = HashSet::from([Target::current().unwrap()]);
        let config_targets: HashSet<Target> = HashSet::from([
            "aarch64-macos".parse().unwrap(),
            "x86_64-linux-static".parse().unwrap(),
        ]);
        let windows_only: HashSet<Target> = HashSet::from([windows]);

        let cases: &[(&Project, Vec<Target>, &HashSet<Target>)] = &[
            (&minimal, vec![], &current_only),
            (&with_config, vec![], &config_targets),
            (&with_config, vec![windows], &windows_only),
        ];

        for (project, cli_targets, expected) in cases {
            assert_eq!(&project.resolve_targets(cli_targets).unwrap(), *expected);
        }
    }

    #[test]
    fn test_resolve_strip_beam() {
        let minimal = Project::load("tests/fixtures/minimal").unwrap();
        let with_config = Project::load("tests/fixtures/with_config").unwrap();

        let cases = [
            (&minimal, None, true),
            (&with_config, None, false),
            (&with_config, Some(true), true),
        ];

        for (project, cli_strip_beam, expected) in cases {
            assert_eq!(project.resolve_strip_beam(cli_strip_beam), expected);
        }
    }

    #[test]
    fn test_resolve_compression_level() {
        let minimal = Project::load("tests/fixtures/minimal").unwrap();
        let with_config = Project::load("tests/fixtures/with_config").unwrap();

        let ok_cases = [
            (&minimal, None, 9),
            (&with_config, None, 3),
            (&with_config, Some(15), 15),
        ];
        for (project, cli_level, expected) in ok_cases {
            assert_eq!(
                project.resolve_compression_level(cli_level).unwrap(),
                expected
            );
        }

        for out_of_range in [0, 23] {
            assert!(
                minimal
                    .resolve_compression_level(Some(out_of_range))
                    .is_err()
            );
        }
    }

    #[test]
    fn test_resolve_full_erts() {
        let minimal = Project::load("tests/fixtures/minimal").unwrap();
        let with_config = Project::load("tests/fixtures/with_config").unwrap();

        let cases = [
            (&minimal, false, false),
            (&with_config, false, true),
            (&minimal, true, true),
        ];

        for (project, cli_full_erts, expected) in cases {
            assert_eq!(project.resolve_full_erts(cli_full_erts), expected);
        }
    }

    #[test]
    fn test_entrypoint_beam_file_suffix() {
        let entry = Entrypoint::new("my_app.cli");
        assert_eq!(entry.beam_file, "my_app@cli.beam");
    }
}
