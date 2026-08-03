// The utz-build-cli binary: a thin dispatcher over utz_build_cli::cmd.
// Crate docs live on the lib target (src/lib.rs); this bin target is
// doc = false so the two don't race for the utz_build_cli doc directory.

use clap::Parser;

use utz_build_cli::cmd;

#[derive(Parser)]
#[command(
    name = "utz-build-cli",
    version,
    about = "μTZ asset generation (the CLI counterpart of a utz-build build.rs)"
)]
enum Cmd {
    /// Generate a .utz asset to disk from explicit knobs
    #[command(visible_alias = "encode")]
    Gen(cmd::encode::Args),
    /// Generate a preset asset from its canonical Config recipe
    GenPreset(cmd::gen_preset::Args),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> utz_build::Result<()> {
    match Cmd::parse() {
        Cmd::Gen(a) => cmd::encode::run(a),
        Cmd::GenPreset(a) => cmd::gen_preset::run(a),
    }
}
