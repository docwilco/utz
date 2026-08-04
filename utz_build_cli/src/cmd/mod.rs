//! One module per `utz_build_cli` subcommand, named after it. Each
//! exposes `Args` (clap; field docs are the `--help` text) and
//! `run(Args)`.

pub mod encode;
pub mod gen_preset;
