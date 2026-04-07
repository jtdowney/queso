use std::process::Command;

use camino_tempfile_ext::prelude::*;
use insta_cmd::{assert_cmd_snapshot, get_cargo_bin};

fn cli() -> Command {
    Command::new(get_cargo_bin("queso"))
}

fn apply_common_filters(settings: &mut insta::Settings) {
    settings.add_filter(r"\x1b\[[0-9;]*m", "");
    settings.add_filter(
        r"(?m)^Location:\n\s+\S+:\d+\n?",
        "Location:\n   [FILE]:[LINE]\n",
    );
    settings.add_filter(r"(?m)^Backtrace omitted\..*(?:\n.*)*", "");
}

#[test]
fn test_help() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    settings.add_filter(r"queso \d+\.\d+\.\d+", "queso [VERSION]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().arg("--help"));
}

#[test]
fn test_build_help() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().args(["build", "--help"]));
}

#[test]
fn test_build_missing_gleam_toml() {
    let dir = camino_tempfile::tempdir().unwrap();
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);

    let canonical = dir.path().canonicalize_utf8().unwrap();
    settings.add_filter(canonical.as_str(), "[TEMPDIR]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(
        cli()
            .args([
                "build",
                "--target",
                "x86_64-linux-static",
                "--erts",
                "/tmp/erts"
            ])
            .current_dir(dir.path())
    );
}

#[test]
fn test_clean_help() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().args(["clean", "--help"]));
}

#[test]
fn test_cache_help() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().args(["cache", "--help"]));
}

#[test]
fn test_cache_clean_help() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().args(["cache", "clean", "--help"]));
}

#[test]
fn test_build_invalid_target() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(
        cli()
            .args(["build", "--target", "mips-linux", "--erts", "/tmp/erts"])
            .current_dir("tests/fixtures/minimal")
    );
}

#[test]
fn test_erts_with_multiple_targets() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(
        cli()
            .args([
                "build",
                "--target",
                "x86_64-linux-static",
                "--target",
                "aarch64-macos",
                "--erts",
                "/tmp/erts",
            ])
            .current_dir("tests/fixtures/minimal")
    );
}

#[test]
fn test_clean_no_artifacts() {
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().arg("clean").current_dir("tests/fixtures/minimal"));
}

#[test]
fn test_clean_with_artifacts() {
    let dir = camino_tempfile::tempdir().unwrap();
    dir.child("gleam.toml")
        .write_str("name = \"test_app\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n")
        .unwrap();
    dir.child("build/queso/test_app-1.0.0-x86_64-linux-static")
        .write_binary(b"fake binary")
        .unwrap();

    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);

    let canonical = dir.path().canonicalize_utf8().unwrap();
    settings.add_filter(regex::escape(canonical.as_str()).as_str(), "[TEMPDIR]");
    settings.add_filter(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().arg("clean").current_dir(dir.path()));

    let build_dir = dir.path().join("build/queso");
    assert!(!build_dir.exists());
}

#[test]
fn test_clean_missing_gleam_toml() {
    let dir = camino_tempfile::tempdir().unwrap();
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);

    let canonical = dir.path().canonicalize_utf8().unwrap();
    settings.add_filter(regex::escape(canonical.as_str()).as_str(), "[TEMPDIR]");
    settings.add_filter(regex::escape(dir.path().as_str()).as_str(), "[TEMPDIR]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(cli().arg("clean").current_dir(dir.path()));
}

#[test]
fn test_cache_clean_no_cache() {
    let dir = camino_tempfile::tempdir().unwrap();
    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);

    settings.add_filter(regex::escape(dir.path().as_str()).as_str(), "[HOME]");
    let canonical = dir.path().canonicalize_utf8().unwrap();
    settings.add_filter(regex::escape(canonical.as_str()).as_str(), "[HOME]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(
        cli()
            .args(["cache", "clean"])
            .env("HOME", dir.path().as_str())
    );
}

#[cfg(target_os = "macos")]
#[test]
fn test_cache_clean_with_cache() {
    let dir = camino_tempfile::tempdir().unwrap();
    dir.child("Library/Caches/queso/erts/dummy.tar.zst")
        .write_binary(b"fake cached erts payload")
        .unwrap();

    let mut settings = insta::Settings::clone_current();
    apply_common_filters(&mut settings);

    settings.add_filter(regex::escape(dir.path().as_str()).as_str(), "[HOME]");
    let canonical = dir.path().canonicalize_utf8().unwrap();
    settings.add_filter(regex::escape(canonical.as_str()).as_str(), "[HOME]");
    let _bound = settings.bind_to_scope();

    assert_cmd_snapshot!(
        cli()
            .args(["cache", "clean"])
            .env("HOME", dir.path().as_str())
    );

    let cache_dir = dir.path().join("Library/Caches/queso");
    assert!(!cache_dir.exists());
}
