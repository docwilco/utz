// The utz_build_cli binary: a thin dispatcher over utz_build_cli::cmd.
// Crate docs live on the lib target (src/lib.rs); this bin target is
// doc = false so the two don't race for the utz_build_cli doc directory.

use clap::Parser;

use utz_build_cli::cmd;

#[derive(Parser)]
#[command(
    name = "utz_build_cli",
    version,
    about = "μTZ asset generation (the CLI counterpart of a utz_build build.rs)"
)]
enum Cmd {
    /// Generates a .utz asset to disk from explicit knobs.
    Gen(cmd::gen::Args),
    /// Generates a preset asset (or every preset) from the canonical
    /// recipe table.
    GenPreset(cmd::gen_preset::Args),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> utz_build::Result<()> {
    match Cmd::parse() {
        Cmd::Gen(args) => cmd::gen::run(args),
        Cmd::GenPreset(args) => cmd::gen_preset::run(args),
    }
}
