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
    fn test_resolve_entry_precedence() {
        let project = Project::load("tests/fixtures/with_config").unwrap();

        let entry = project.resolve_entry(None);
        assert_eq!(entry.entry_module, "my_app@cli");

        let entry = project.resolve_entry(Some("custom.mod"));
        assert_eq!(entry.entry_module, "custom@mod");
    }

    #[test]
    fn test_resolve_entry_falls_back_to_package_name() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        let entry = project.resolve_entry(None);
        assert_eq!(entry.entry_module, "my_app");
    }

    #[test]
    fn test_resolve_targets_cli_overrides_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        let cli_targets: Vec<Target> = vec!["x86_64-windows".parse().unwrap()];
        let targets = project.resolve_targets(&cli_targets).unwrap();
        let names: HashSet<String> = targets.iter().map(ToString::to_string).collect();
        assert_eq!(names.len(), 1);
        assert!(names.contains("x86_64-windows"));
    }

    #[test]
    fn test_resolve_targets_uses_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        let targets = project.resolve_targets(&[]).unwrap();
        let names: HashSet<String> = targets.iter().map(ToString::to_string).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains("aarch64-macos"));
        assert!(names.contains("x86_64-linux-static"));
    }

    #[test]
    fn test_resolve_targets_falls_back_to_current() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        let targets = project.resolve_targets(&[]).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&Target::current().unwrap()));
    }

    #[test]
    fn test_resolve_strip_beam_defaults_to_true() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert!(project.resolve_strip_beam(None));
    }

    #[test]
    fn test_resolve_strip_beam_uses_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert!(!project.resolve_strip_beam(None));
    }

    #[test]
    fn test_resolve_strip_beam_cli_overrides_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert!(project.resolve_strip_beam(Some(true)));
    }

    #[test]
    fn test_resolve_compression_level_defaults_to_9() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert_eq!(project.resolve_compression_level(None).unwrap(), 9);
    }

    #[test]
    fn test_resolve_compression_level_uses_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert_eq!(project.resolve_compression_level(None).unwrap(), 3);
    }

    #[test]
    fn test_resolve_compression_level_cli_overrides_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert_eq!(project.resolve_compression_level(Some(15)).unwrap(), 15);
    }

    #[test]
    fn test_resolve_full_erts_defaults_to_false() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert!(!project.resolve_full_erts(false));
    }

    #[test]
    fn test_resolve_full_erts_uses_config() {
        let project = Project::load("tests/fixtures/with_config").unwrap();
        assert!(project.resolve_full_erts(false));
    }

    #[test]
    fn test_resolve_full_erts_cli_overrides_config() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert!(project.resolve_full_erts(true));
    }

    #[test]
    fn test_resolve_compression_level_rejects_out_of_range() {
        let project = Project::load("tests/fixtures/minimal").unwrap();
        assert!(project.resolve_compression_level(Some(0)).is_err());
        assert!(project.resolve_compression_level(Some(23)).is_err());
    }

    #[test]
    fn test_entrypoint_dots_become_at_signs() {
        fn prop(parts: Vec<String>) -> bool {
            let parts: Vec<String> = parts
                .into_iter()
                .filter(|s| {
                    !s.is_empty()
                        && s.starts_with(|c: char| c.is_ascii_lowercase())
                        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                .take(5)
                .collect();
            if parts.is_empty() {
                return true;
            }
            let module = parts.join(".");
            let entry = Entrypoint::new(&module);
            let expected_erlang = parts.join("@");
            entry.entry_module == expected_erlang
                && entry.beam_file == format!("{expected_erlang}.beam")
        }
        quickcheck::quickcheck(prop as fn(Vec<String>) -> bool);
    }
}
