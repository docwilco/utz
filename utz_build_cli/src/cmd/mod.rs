//! Each `utz_build_cli` subcommand gets one module here, named after it.
//! Each module exposes `Args` (clap; the field docs are the `--help` text)
//! and `run(Args)`.

pub mod encode;
pub mod gen_preset;
