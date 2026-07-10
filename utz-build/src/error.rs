//! Builder error type — the workspace error pattern (see `utz::Error`):
//! `derive_more` derives, foreign errors enter via `derive_more::From`,
//! domain variants are `#[from(skip)]`. Library paths use typed variants;
//! the cmd/* measurement tools may use [`Error::Msg`] for one-off messages.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, derive_more::Display, derive_more::Error, derive_more::From)]
pub enum Error {
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
    /// boxed: `ureq::Error` is large
    Http(Box<ureq::Error>),
    Tiff(tiff::TiffError),
    Fgb(flatgeobuf::Error),
    Geozero(flatgeobuf::geozero::error::GeozeroError),
    Encode(utz_encode::Error),
    Utz(utz::Error),
    #[from(skip)]
    #[display("unknown dataset {ds:?}: use [land-]now|1970|all")]
    UnknownDataset { ds: String },
    #[from(skip)]
    #[display("no legacy .fgb for dataset {ds}")]
    NoLegacyFgb { ds: String },
    #[from(skip)]
    #[display("no /releases/tag/ redirect (status {status})")]
    NoReleaseRedirect { status: u16 },
    #[from(skip)]
    #[display("no geojson entry in {}", path.display())]
    NoGeojsonEntry { path: PathBuf },
    #[from(skip)]
    #[display("no filename in url {url}")]
    NoFilename { url: String },
    #[from(skip)]
    #[display("no .tif in {}", zip.display())]
    NoTif { zip: PathBuf },
    #[from(skip)]
    #[display("missing geotransform")]
    MissingGeotransform,
    #[from(skip)]
    #[display("unexpected GHS-POP sample format {format}")]
    BadSampleFormat { format: String },
    #[from(skip)]
    #[display("bad density sidecar: {_0}")]
    BadSidecar(#[error(not(source))] &'static str),
    #[from(skip)]
    #[display("no OUT_DIR (not in a build.rs?) — set .out_path()")]
    NoOutDir,
    /// one-off messages in the cmd/* measurement tools
    #[from(skip)]
    #[display("{_0}")]
    Msg(#[error(not(source))] String),
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(Box::new(e))
    }
}

/// anyhow-style guard returning a typed [`Error`].
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $err:expr) => {
        if !($cond) {
            return Err($err.into());
        }
    };
}
