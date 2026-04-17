use std::env;

use camino::Utf8PathBuf;
use eyre::{Result, bail, eyre};
use queso::pack;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  pack-launcher    regenerate launcher-source.tar.zst at the repo root
";

fn main() -> Result<()> {
    let task = env::args().nth(1);
    match task.as_deref() {
        Some("pack-launcher") => pack_launcher(),
        Some("-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown task `{other}`\n\n{USAGE}"),
        None => bail!("missing task\n\n{USAGE}"),
    }
}

fn pack_launcher() -> Result<()> {
    let xtask_manifest = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = xtask_manifest
        .parent()
        .ok_or_else(|| eyre!("xtask manifest {xtask_manifest} has no parent"))?
        .to_path_buf();
    let archive_path = repo_root.join("launcher-source.tar.zst");
    pack::pack_launcher_source(&repo_root, &archive_path)?;
    println!("wrote {archive_path}");
    Ok(())
}
