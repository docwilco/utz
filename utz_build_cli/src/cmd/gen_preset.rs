//! Generates one preset asset, or every preset, from the canonical
//! recipe table. The command drives `utz_build::Config::from_recipe()`
//! over `utz_build::presets`, so the recipes exist in exactly one place
//! and `scripts/gen-presets.sh` cannot drift from the table (the data
//! crates' build.rs recipe guards verify the result).
//!
//! ```text
//! utz_build_cli gen-preset [tiny|tiny-static|compact|balanced|accurate]
//! ```

use std::path::PathBuf;

use utz_build::presets::{self, Recipe};
use utz_build::Config;

#[derive(clap::Args)]
pub struct Args {
    /// The preset name; omit it to generate every preset.
    preset: Option<String>,
    /// The output path (the default is `utz_data_<preset>/data/<preset>.utz`
    /// with `-` as `_` in the crate dir, relative to the current directory).
    #[arg(long, short, requires = "preset")]
    out: Option<PathBuf>,
}

/// # Errors
/// The command fails on an unknown preset name, a dataset load or encode
/// failure, or an I/O error writing the asset and its guard file.
pub fn run(args: Args) -> utz_build::Result<()> {
    if let Some(name) = args.preset {
        let recipe = presets::by_name(&name).ok_or_else(|| {
            let known = presets::ALL.map(|recipe| recipe.name).join("|");
            utz_build::Error::Msg(format!("unknown preset {name:?}: use {known}"))
        })?;
        generate(recipe, args.out)
    } else {
        for recipe in &presets::ALL {
            generate(recipe, None)?;
        }
        Ok(())
    }
}

/// Generates one preset asset, deriving the default output path from the
/// recipe name when none is given.
fn generate(recipe: &Recipe, out: Option<PathBuf>) -> utz_build::Result<()> {
    let out = if let Some(out) = out {
        out
    } else {
        let dir = PathBuf::from(format!("utz_data_{}", recipe.name.replace('-', "_")));
        if !dir.is_dir() {
            return Err(utz_build::Error::Msg(format!(
                "default output {}/data/ expects the μTZ checkout root as the \
                 current directory; pass --out",
                dir.display()
            )));
        }
        dir.join("data").join(format!("{}.utz", recipe.name))
    };
    let path = Config::from_recipe(recipe).out_path(out).generate()?;
    println!("wrote {}", path.display());
    Ok(())
}
