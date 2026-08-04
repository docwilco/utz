//! Generate a preset asset from its canonical recipe: drives
//! `utz_build::Config::<preset>()` directly, so the recipe exists in
//! exactly one place and `scripts/gen-presets.sh` cannot drift from the
//! `Config` constructors (the data crates' build.rs recipe guards verify
//! the result).
//!
//! ```text
//! utz_build_cli gen-preset <tiny|tiny-static|compact|balanced|accurate>
//! ```

use std::path::PathBuf;

use utz_build::{Codec, Config};

#[derive(clap::Args)]
pub struct Args {
    /// preset name: tiny|tiny-static|compact|balanced|accurate
    preset: String,
    /// output path (default: `utz_data_<preset>/data/<preset>.utz` with
    /// `-` as `_` in the crate dir, relative to the current directory)
    #[arg(long, short)]
    out: Option<PathBuf>,
}

/// # Errors
/// Unknown preset name, dataset load or encode failure, or I/O writing
/// the asset and its guard file.
pub fn run(args: Args) -> utz_build::Result<()> {
    let config = match args.preset.as_str() {
        "tiny" => Config::tiny(),
        "tiny-static" => Config::tiny().codec(Codec::Uncompressed),
        "compact" => Config::compact(),
        "balanced" => Config::balanced(),
        "accurate" => Config::accurate(),
        other => {
            return Err(utz_build::Error::Msg(format!(
                "unknown preset {other:?}: use tiny|tiny-static|compact|balanced|accurate"
            )))
        }
    };
    let out = if let Some(out) = args.out {
        out
    } else {
        let dir = PathBuf::from(format!("utz_data_{}", args.preset.replace('-', "_")));
        if !dir.is_dir() {
            return Err(utz_build::Error::Msg(format!(
                "default output {}/data/ expects the μTZ checkout root as the \
                 current directory; pass --out",
                dir.display()
            )));
        }
        dir.join("data").join(format!("{}.utz", args.preset))
    };
    let path = config.out_path(out).generate()?;
    println!("wrote {}", path.display());
    Ok(())
}
