//! Each `utz_dev_cli` subcommand gets one module here, named after it.
//! Each module exposes `Args` (clap; the field docs are the `--help` text)
//! and `run(Args)`. The module docs carry the question the subcommand
//! answers and its method; the crate root groups them by area.

pub mod accuracy;
pub mod amscan;
pub mod csr_sweep;
pub mod density_compare;
pub mod density_probe;
pub mod dominant_cost;
pub mod fixedwidth_size;
pub mod geoquant;
pub mod grid2mem;
pub mod grid_bench;
pub mod gridsweep;
pub mod imagepack_size;
pub mod pip_bench;
pub mod polygrid_probe;
pub mod quant_clean;
pub mod quant_size;
pub mod rdp_sweep;
pub mod roundtrip;
pub mod size_table;
pub mod visualize;
pub mod whittle;
pub mod window_sweep;
