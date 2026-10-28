use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use litellm_model_catalog::Located;

use crate::source::Source;

/// Main's `RETRYABLE_FETCH_STATUS_CODES`
const TRANSIENT_STATUSES: [u16; 5] = [429, 500, 502, 503, 504];

#[derive(Clone, Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error(Arc<Repr>);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Repr {
    #[error("{text:?} is not a URL")]
    InvalidUrl {
        text: String,
        source: url::ParseError,
    },
    #[error("{text:?} is not a local file URL")]
    NotAFilePath { text: String },
    #[error("{text:?} is not an s3://bucket/key URL")]
    InvalidS3Url { text: String },
    #[error("model cost map URLs are http, https, file or s3, not {scheme}")]
    UnsupportedScheme { scheme: String },
    #[error("fetching {from} failed")]
    Http {
        from: Source,
        source: litellm_http::Error,
    },
    #[error("reading {} failed", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("fetching {from} failed")]
    S3 {
        from: Source,
        source: litellm_error::Source,
    },
    #[error("fetching {from} timed out")]
    S3Timeout { from: Source },
    #[error("{from} redirected twice, last to {region}")]
    S3Redirect { from: Source, region: String },
    #[error("reading {from} failed")]
    S3Body {
        from: Source,
        source: litellm_error::Source,
    },
    #[error("HTTP {status} from {from}")]
    Status {
        from: Source,
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("{from} is not modified")]
    NotModified { from: Source },
    #[error("model cost map from {from} is unusable")]
    Unusable { from: Source, source: Located },
    #[error("gave up after {attempts} attempts")]
    Exhausted { attempts: u32, source: Error },
    #[error("revalidation is not awaiting {0}")]
    OutOfOrder(Awaiting),
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum Awaiting {
    #[error("a reply")]
    Reply,
    #[error("a sleep")]
    Sleep,
}

impl From<Repr> for Error {
    fn from(repr: Repr) -> Self {
        Self(Arc::new(repr))
    }
}

impl Error {
    /// A later attempt at the same source may succeed
    pub fn transient(&self) -> bool {
        match &*self.0 {
            Repr::Http { .. }
            | Repr::File { .. }
            | Repr::S3 { .. }
            | Repr::S3Timeout { .. }
            | Repr::S3Redirect { .. }
            | Repr::S3Body { .. } => true,
            Repr::Status { status, .. } => TRANSIENT_STATUSES.contains(status),
            Repr::InvalidUrl { .. }
            | Repr::NotAFilePath { .. }
            | Repr::InvalidS3Url { .. }
            | Repr::UnsupportedScheme { .. }
            | Repr::NotModified { .. }
            | Repr::Unusable { .. }
            | Repr::Exhausted { .. }
            | Repr::OutOfOrder(_) => false,
        }
    }

    /// The wait the source asked for with `Retry-After`
    pub fn retry_after(&self) -> Option<Duration> {
        match &*self.0 {
            Repr::Status { retry_after, .. } => *retry_after,
            Repr::InvalidUrl { .. }
            | Repr::NotAFilePath { .. }
            | Repr::InvalidS3Url { .. }
            | Repr::UnsupportedScheme { .. }
            | Repr::Http { .. }
            | Repr::File { .. }
            | Repr::S3 { .. }
            | Repr::S3Timeout { .. }
            | Repr::S3Redirect { .. }
            | Repr::S3Body { .. }
            | Repr::NotModified { .. }
            | Repr::Unusable { .. }
            | Repr::Exhausted { .. }
            | Repr::OutOfOrder(_) => None,
        }
    }

    /// The status the source answered with, for the last attempt when retries ran out
    pub fn status(&self) -> Option<u16> {
        match &*self.0 {
            Repr::Status { status, .. } => Some(*status),
            Repr::Exhausted { source, .. } => source.status(),
            Repr::InvalidUrl { .. }
            | Repr::NotAFilePath { .. }
            | Repr::InvalidS3Url { .. }
            | Repr::UnsupportedScheme { .. }
            | Repr::Http { .. }
            | Repr::File { .. }
            | Repr::S3 { .. }
            | Repr::S3Timeout { .. }
            | Repr::S3Redirect { .. }
            | Repr::S3Body { .. }
            | Repr::NotModified { .. }
            | Repr::Unusable { .. }
            | Repr::OutOfOrder(_) => None,
        }
    }
}
