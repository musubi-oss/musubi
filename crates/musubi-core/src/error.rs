use serde::Serialize;

/// Canonical error for every subsystem. Maps 1:1 onto HTTP/gRPC status codes at the edge.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Stable machine-readable code used on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    NotFound,
    AlreadyExists,
    InvalidArgument,
    Unauthenticated,
    PermissionDenied,
    Conflict,
    ResourceExhausted,
    Unavailable,
    Internal,
}

impl Error {
    pub fn code(&self) -> Code {
        match self {
            Error::NotFound(_) => Code::NotFound,
            Error::AlreadyExists(_) => Code::AlreadyExists,
            Error::InvalidArgument(_) => Code::InvalidArgument,
            Error::Unauthenticated(_) => Code::Unauthenticated,
            Error::PermissionDenied(_) => Code::PermissionDenied,
            Error::Conflict(_) => Code::Conflict,
            Error::ResourceExhausted(_) => Code::ResourceExhausted,
            Error::Unavailable(_) => Code::Unavailable,
            Error::Internal(_) => Code::Internal,
        }
    }

    pub fn http_status(&self) -> u16 {
        match self.code() {
            Code::NotFound => 404,
            Code::AlreadyExists | Code::Conflict => 409,
            Code::InvalidArgument => 400,
            Code::Unauthenticated => 401,
            Code::PermissionDenied => 403,
            Code::ResourceExhausted => 429,
            Code::Unavailable => 503,
            Code::Internal => 500,
        }
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Error::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::InvalidArgument(e.to_string())
    }
}
