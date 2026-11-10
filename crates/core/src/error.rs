use std::fmt;

/// Failures a caller can show to a person or put in a JSON envelope.
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Json(String),
    InvalidSlug(String),
    EmptyField(&'static str),
    TaskExists(String),
    NoSuchTask(String),
    AlreadyClaimed,
    NoSuchStep(i64),
    AlreadyReported(String),
    DatabaseBusy,
    BadStatus(String),
    BadEvent(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(formatter, "sqlite: {err}"),
            Self::Io(err) => write!(formatter, "io: {err}"),
            Self::Json(err) => write!(formatter, "json: {err}"),
            Self::InvalidSlug(slug) => write!(formatter, "invalid slug: {slug}"),
            Self::EmptyField(field) => write!(formatter, "{field} is required"),
            Self::TaskExists(slug) => write!(formatter, "task already exists: {slug}"),
            Self::NoSuchTask(slug) => write!(formatter, "no such task: {slug}"),
            Self::AlreadyClaimed => formatter.write_str("already claimed"),
            Self::NoSuchStep(position) => write!(formatter, "no such step: {position}"),
            Self::AlreadyReported(sha) => write!(formatter, "already reported for {sha}"),
            Self::DatabaseBusy => formatter.write_str("database busy"),
            Self::BadStatus(status) => write!(formatter, "invalid status: {status}"),
            Self::BadEvent(message) => write!(formatter, "invalid event: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(err) => Some(err),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        match &err {
            rusqlite::Error::SqliteFailure(ffi, _)
                if ffi.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                Self::DatabaseBusy
            }
            _ => Self::Sqlite(err),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}
