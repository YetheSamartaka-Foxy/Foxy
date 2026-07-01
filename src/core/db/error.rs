//! Database error type for the seam (plan.md §5.1).
//!
//! Before the Turso cutover this was a re-export of `sea_orm::DbErr`. SeaORM is
//! gone now, so `DbErr` is a small crate-owned enum carrying only the variants
//! the codebase actually constructs (`Custom` for engine/seam errors,
//! `RecordNotFound` for empty lookups). Turso engine errors are funneled through
//! `From<turso::Error>` so `?` works on the raw `turso` API inside the seam.

use std::fmt;

/// Storage-neutral database error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DbErr {
    /// Any engine/seam failure carrying a human-readable message.
    Custom(String),
    /// A lookup that was expected to return a row found none.
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
        DbErr::Custom(format!("turso: {err}"))
    }
}
