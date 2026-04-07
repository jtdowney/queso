mod cli;

use std::{collections::HashMap, env, fs};

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand};
use eyre::Result;
use indicatif::HumanBytes;
use queso::{
    Metadata,
    erl::Erl,
    erts::{self, ErtsResolution},
    payload,
    project::{self, Project},
    target::Target,
    tree_shake,
};

#[derive(Debug, Parser)]
#[command(
    name = "queso",
    version,
    about = "Package Gleam apps into native executables"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Build a distributable executable
    Build(BuildArgs),
    /// Remove build artifacts
    Clean,
    /// Manage the global cache
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Remove cached ERTS downloads and payloads
    Clean,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Target platforms (e.g., x86_64-linux-static, aarch64-macos; defaults to current)
    #[arg(long)]
    target: Vec<Target>,

    /// Path to Erlang/OTP installation (auto-downloaded if omitted)
    #[arg(long)]
    erts: Option<Utf8PathBuf>,

    /// Entrypoint module (default: package root module)
    #[arg(long)]
    entry: Option<String>,

    /// Bundle the entire ERTS (skip tree shaking)
    #[arg(long)]
    full_erts: bool,

    /// Strip debug info from BEAM files (default)
    #[arg(long, group = "strip_beam_group")]
    strip_beam: bool,

    /// Don't strip debug info from BEAM files
    #[arg(long = "no-strip-beam", group = "strip_beam_group")]
    no_strip_beam: bool,

    /// Zstd compression level (1-22, default: 9)
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..=22))]
    compression_level: Option<i32>,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    match cli.command {
        CliCommand::Build(args) => build(&args),
        CliCommand::Clean => clean(),
        CliCommand::Cache { command } => match command {
            CacheCommand::Clean => cache_clean(),
        },
    }
}

struct ResolvedBuild {
    project: Project,
    entry: project::Entrypoint,
    strip_beam: bool,
    compression_level: i32,
    erts_override: Option<Utf8PathBuf>,
    full_erts: bool,
    output_dir: Utf8PathBuf,
    shipment_apps: HashMap<String, Vec<String>>,
    app_payload_path: Utf8PathBuf,
    app_hash: String,
}

fn build(args: &BuildArgs) -> Result<()> {
    let cwd = Utf8PathBuf::from_path_buf(env::current_dir()?)
        .map_err(|p| eyre::eyre!("non-UTF-8 working directory: {}", p.display()))?;
    let project = Project::load(&cwd)?;
    let targets = project.resolve_targets(&args.target)?;

    if args.erts.is_some() && targets.len() > 1 {
        eyre::bail!(
            "--erts cannot be used with multiple targets (each target needs a platform-specific ERTS)"
        );
    }

    let cli_strip_beam = match (args.strip_beam, args.no_strip_beam) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let strip_beam = project.resolve_strip_beam(cli_strip_beam);
    let compression_level = project.resolve_compression_level(args.compression_level)?;
    let zig_version = queso::check_zig()?;
    let gleam_version = queso::check_gleam()?;
    let entry = project.resolve_entry(args.entry.as_deref());
    let full_erts = project.resolve_full_erts(args.full_erts);

    let printer = cli::Printer::stderr();

    printer.status(
        "Building",
        &format!("{} v{}", project.name, project.version),
    )?;
    printer.detail("Zig", &zig_version)?;
    printer.detail("Gleam", &gleam_version)?;
    printer.detail("Entry", &entry.entry_module)?;

    let output_dir = project.root.join("build").join("queso");
    fs::create_dir_all(&output_dir)?;

    let mut erl = Erl::spawn()?;

    let sp = printer.spinner("Exporting", "erlang-shipment");
    queso::gleam_build(&project.root)?;
    queso::gleam_validate_entrypoint(&project.root, &entry.beam_file)?;
    sp.finish_and_clear();
    printer.status("Exported", "erlang-shipment")?;

    let shipment_dir = queso::gleam_erlang_build_dir(&project.root);
    let shipment_apps = erl.walk_app_dependencies(&shipment_dir)?;

    let work_dir = camino_tempfile::tempdir()?;
    let app_payload_path = work_dir.path().join("app.tar.zst");
    let app_hash = payload::assemble_app(
        &mut erl,
        &app_payload_path,
        &shipment_dir,
        strip_beam,
        compression_level,
    )?;

    let app_size = fs::metadata(&app_payload_path)?.len();
    printer.detail(
        "App size",
        &format!("{} (sha256: {app_hash})", HumanBytes(app_size)),
    )?;

    let resolved = ResolvedBuild {
        project,
        entry,
        strip_beam,
        compression_level,
        erts_override: args.erts.clone(),
        full_erts,
        output_dir,
        shipment_apps,
        app_payload_path,
        app_hash,
    };

    for target in &targets {
        build_target(&printer, &mut erl, &resolved, *target)?;
    }

    Ok(())
}

fn clean() -> Result<()> {
    let printer = cli::Printer::stderr();
    let cwd = Utf8PathBuf::from_path_buf(env::current_dir()?)
        .map_err(|p| eyre::eyre!("non-UTF-8 working directory: {}", p.display()))?;
    let project = Project::load(&cwd)?;
    let dir = project.root.join("build").join("queso");

    if !dir.exists() {
        printer.status("Clean", "no build artifacts found")?;
        return Ok(());
    }

    let size: u64 = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();

    fs::remove_dir_all(&dir)?;
    printer.status("Removed", &format!("{} ({})", dir, HumanBytes(size)))?;

    Ok(())
}

fn cache_clean() -> Result<()> {
    let printer = cli::Printer::stderr();
    let dir = queso::cache_dir()?;

    if !dir.exists() {
        printer.status("Clean", "no cache directory found")?;
        return Ok(());
    }

    let size: u64 = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();

    fs::remove_dir_all(&dir)?;
    printer.status("Removed", &format!("{} ({})", dir, HumanBytes(size)))?;

    Ok(())
}

fn resolve_erts(
    printer: &cli::Printer,
    erl: &mut Erl,
    erts_override: Option<&Utf8Path>,
    target: Target,
) -> Result<Utf8PathBuf> {
    if let Some(path) = erts_override {
        return Ok(path.to_path_buf());
    }

    let otp_version = erl.get_otp_version()?;
    let sp = printer.spinner("Downloading", "ERTS");
    let resolution = erts::resolve(&otp_version, &target)?;
    sp.finish_and_clear();

    match resolution {
        ErtsResolution::Cached { path, otp_version } => {
            printer.detail("OTP", &otp_version)?;
            printer.detail("Cached", path.as_ref())?;
            Ok(path)
        }
        ErtsResolution::Downloaded {
            path,
            otp_version,
            url,
            hash,
        } => {
            printer.status("Downloaded", "ERTS")?;
            printer.detail("OTP", &otp_version)?;
            printer.detail("Download", &url)?;
            printer.detail("Hash", &hash)?;
            Ok(path)
        }
    }
}

fn build_target(
    printer: &cli::Printer,
    erl: &mut Erl,
    resolved: &ResolvedBuild,
    target: Target,
) -> Result<()> {
    printer.header(&target.to_string())?;

    let erts_path = resolve_erts(printer, erl, resolved.erts_override.as_deref(), target)?;

    let erts = erts::validate(&erts_path, &target)?;
    printer.detail("ERTS", &format!("v{}", erts.version))?;

    let allowed_otp_apps = if resolved.full_erts {
        None
    } else {
        let erts_lib_dir = erts_path.join("lib");
        let erts_apps = erl.walk_app_dependencies(&erts_lib_dir)?;
        let required = tree_shake::resolve(&resolved.shipment_apps, &erts_apps);
        tree_shake::validate(&required, &resolved.shipment_apps, &erts_apps)?;
        let mut app_names: Vec<&str> = required.iter().map(String::as_str).collect();
        app_names.sort_unstable();

        printer.detail("OTP apps", &app_names.join(", "))?;
        Some(required)
    };

    let work_dir = camino_tempfile::tempdir()?;

    let erts_payload_path = work_dir.path().join("erts.tar.zst");
    let erts_hash = payload::assemble_erts(
        erl,
        &erts_payload_path,
        &erts,
        allowed_otp_apps.as_ref(),
        resolved.strip_beam,
        &target,
        resolved.compression_level,
    )?;

    let erts_size = fs::metadata(&erts_payload_path)?.len();
    printer.detail(
        "ERTS size",
        &format!("{} (sha256: {erts_hash})", HumanBytes(erts_size)),
    )?;

    let boot_path = queso::find_boot_file(&erts_path)?;

    let meta = Metadata {
        name: resolved.project.name.clone(),
        version: resolved.project.version.clone(),
        entry_module: resolved.entry.entry_module.clone(),
        target,
        erts_version: erts.version.clone(),
        erts_hash,
        app_hash: resolved.app_hash.clone(),
        boot_path,
    };

    let filename =
        queso::output_filename(&resolved.project.name, &resolved.project.version, &target);
    let final_path = resolved.output_dir.join(&filename);

    let sp = printer.spinner("Compiling", "launcher");
    queso::compile_launcher(
        &target,
        &erts_payload_path,
        &resolved.app_payload_path,
        &meta,
        &final_path,
    )?;
    sp.finish_and_clear();
    printer.status("Compiled", "launcher")?;

    let binary_size = fs::metadata(&final_path)?.len();
    printer.detail("Binary", HumanBytes(binary_size).to_string().as_str())?;
    printer.status("Built", final_path.as_ref())?;

    Ok(())
}
