use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DbErr {
    Custom(String),

    RecordNotFound(String),
}

impl fmt::Display for DbErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbErr::Custom(msg) => write!(f, "{msg}"),
            DbErr::RecordNotFound(what) => write!(f, "record not found: {what}"),
        }
    }
}

impl std::error::Error for DbErr {}

impl From<turso::Error> for DbErr {
    fn from(err: turso::Error) -> Self {
        match err {
            // Retries are classified by message, and this variant's text names
            // neither busy nor a conflict.
            turso::Error::BusySnapshot(msg) => {
                DbErr::Custom(format!("turso: busy snapshot: {msg}"))
            }
            err => DbErr::Custom(format!("turso: {err}")),
        }
    }
}
