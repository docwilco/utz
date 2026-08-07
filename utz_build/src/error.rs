//! The builder error type follows the workspace error pattern (see
//! `utz::Error`): the derives come from `derive_more`, foreign errors
//! enter via `derive_more::From`, and domain variants are
//! `#[from(skip)]`. Library paths
//! use typed variants; the measurement tools in `utz_dev_cli` may use
//! [`Error::Msg`] for one-off messages.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can fail while fetching sources or generating an asset.
#[derive(Debug, derive_more::Display, derive_more::Error, derive_more::From)]
pub enum Error {
    /// File or network I/O.
    Io(std::io::Error),
    /// A downloaded source archive would not unzip.
    Zip(zip::result::ZipError),
    /// Source `GeoJSON` would not parse.
    Json(serde_json::Error),
    /// An HTTP failure. The error is boxed because `ureq::Error` is
    /// large.
    Http(Box<ureq::Error>),
    Tiff(tiff::TiffError),
    Encode(utz_encode::Error),
    #[cfg(feature = "utz-error")]
    Utz(utz::Error),
    #[from(skip)]
    #[display("unknown dataset {ds:?}: use [land-]now|1970|all")]
    UnknownDataset {
        ds: String,
    },
    #[from(skip)]
    #[display("no /releases/tag/ redirect (status {status})")]
    NoReleaseRedirect {
        status: u16,
    },
    #[from(skip)]
    #[display("no geojson entry in {}", path.display())]
    NoGeojsonEntry {
        path: PathBuf,
    },
    #[from(skip)]
    #[display("no filename in url {url}")]
    NoFilename {
        url: String,
    },
    #[from(skip)]
    #[display("no .tif in {}", zip.display())]
    NoTif {
        zip: PathBuf,
    },
    #[from(skip)]
    #[display("missing geotransform")]
    MissingGeotransform,
    #[from(skip)]
    #[display("unexpected GHS-POP sample format {format}")]
    BadSampleFormat {
        format: String,
    },
    #[from(skip)]
    #[display("bad density sidecar: {_0}")]
    BadSidecar(#[error(not(source))] &'static str),
    #[from(skip)]
    #[display("no OUT_DIR (not in a build.rs?) — set .out_path()")]
    NoOutDir,
    /// A one-off message from the `utz_dev_cli` measurement tools.
    #[from(skip)]
    #[display("{_0}")]
    Msg(#[error(not(source))] String),
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        Error::Http(Box::new(error))
    }
}

/// An anyhow-style guard that returns a typed [`Error`].
#[macro_export]
macro_rules! ensure {
    ($cond:expr_2021, $err:expr_2021) => {
        if !($cond) {
            return Err($err.into());
        }
    };
}
