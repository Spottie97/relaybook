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
    CannotBlockDone,
    SelfDependency,
    DependencyCycle(String),
    WaitingOn(String),
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
            Self::CannotBlockDone => formatter.write_str("cannot block a done task"),
            Self::SelfDependency => formatter.write_str("cannot depend on itself"),
            Self::DependencyCycle(path) => write!(formatter, "dependency cycle: {path}"),
            Self::WaitingOn(slugs) => write!(formatter, "waiting on: {slugs}"),
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

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn messages_match_the_cli_contract() {
        assert_eq!(Error::AlreadyClaimed.to_string(), "already claimed");
        assert_eq!(
            Error::CannotBlockDone.to_string(),
            "cannot block a done task"
        );
        assert_eq!(Error::SelfDependency.to_string(), "cannot depend on itself");
        assert_eq!(
            Error::DependencyCycle("a -> b -> a".to_string()).to_string(),
            "dependency cycle: a -> b -> a"
        );
        assert_eq!(
            Error::WaitingOn("auth, ledger".to_string()).to_string(),
            "waiting on: auth, ledger"
        );
        assert_eq!(
            Error::AlreadyReported("abc".to_string()).to_string(),
            "already reported for abc"
        );
        assert_eq!(
            Error::NoSuchTask("auth".to_string()).to_string(),
            "no such task: auth"
        );
        assert_eq!(Error::NoSuchStep(2).to_string(), "no such step: 2");
        assert_eq!(
            Error::InvalidSlug("Auth".to_string()).to_string(),
            "invalid slug: Auth"
        );
        assert_eq!(Error::EmptyField("title").to_string(), "title is required");
        assert_eq!(
            Error::TaskExists("auth".to_string()).to_string(),
            "task already exists: auth"
        );
        assert_eq!(Error::DatabaseBusy.to_string(), "database busy");
        assert_eq!(
            Error::BadStatus("nope".to_string()).to_string(),
            "invalid status: nope"
        );
    }
}
