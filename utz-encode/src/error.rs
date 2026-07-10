//! Encoder error type — the workspace error pattern (see `utz::Error`):
//! `derive_more` derives, foreign errors enter via `derive_more::From`,
//! domain variants are `#[from(skip)]`.

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, derive_more::Display, derive_more::Error, derive_more::From)]
pub enum Error {
    /// zstd/brotli compressor I/O failure — they write to memory, so this
    /// signals a codec bug rather than an environment problem
    #[display("compression failed: {_0}")]
    Compress(std::io::Error),
    /// no_std `lzma_rust2::Error` isn't `core::error::Error` — stringified
    #[from(skip)]
    #[display("xz compression failed: {_0}")]
    Xz(#[error(not(source))] String),
    #[from(skip)]
    #[display("utz-encode built without the `zstd` feature")]
    ZstdNotCompiled,
    #[from(skip)]
    #[display("quant_bits must be 16/24/32 (got {bits})")]
    QuantBits { bits: u32 },
    #[from(skip)]
    #[display("grid_deg must be within 0.1\u{2013}45 (got {deg})")]
    GridDeg { deg: f64 },
    /// A count or byte length exceeds the width the container format stores
    /// it at — fail loudly instead of an `as` wrap silently corrupting tables.
    #[from(skip)]
    #[display("{what} ({n}) exceeds format limit {max}")]
    FormatLimit { what: &'static str, n: usize, max: usize },
    #[from(skip)]
    #[display("grid {deg}°: {n} unique candidate lists exceed the 15-bit tag space — coarsen the grid")]
    GridLists { deg: f64, n: usize },
    #[from(skip)]
    #[display("grid {deg}°: {n} interned list ids overflow the u16 offset table — coarsen the grid")]
    GridListIds { deg: f64, n: usize },
    #[from(skip)]
    #[display("visvalingam's knob is an area, not ε — build the topology with topo::build_topology_algo and use payload_from_topology")]
    VisvalingamEps,
}

/// anyhow-style guard returning a typed [`Error`].
macro_rules! ensure {
    ($cond:expr, $err:expr) => {
        if !($cond) {
            return Err($err);
        }
    };
}
pub(crate) use ensure;
