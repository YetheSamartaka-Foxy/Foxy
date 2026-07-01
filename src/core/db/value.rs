//! Storage-neutral SQL value type for the DB seam (plan.md §5.1, Phase 1).
//!
//! Call sites build parameter lists as `Vec<DbValue>` (see the [`params!`] macro)
//! and the seam converts them to the engine's native `turso::Value`. Keeping a
//! single neutral type here is what lets the ~40 call sites stay independent of
//! the engine's concrete value type.

/// A single bound SQL parameter, independent of the storage engine.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DbValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for DbValue {
    fn from(v: i64) -> Self {
        DbValue::Int(v)
    }
}

impl From<i32> for DbValue {
    fn from(v: i32) -> Self {
        DbValue::Int(v as i64)
    }
}

impl From<u32> for DbValue {
    fn from(v: u32) -> Self {
        DbValue::Int(i64::from(v))
    }
}

impl From<u64> for DbValue {
    fn from(v: u64) -> Self {
        // Foxy ids are non-negative and fit in i64; SQLite/Turso store INTEGER as i64.
        DbValue::Int(v as i64)
    }
}

impl From<bool> for DbValue {
    fn from(v: bool) -> Self {
        DbValue::Int(i64::from(v))
    }
}

impl From<f64> for DbValue {
    fn from(v: f64) -> Self {
        DbValue::Real(v)
    }
}

impl From<String> for DbValue {
    fn from(v: String) -> Self {
        DbValue::Text(v)
    }
}

impl From<&str> for DbValue {
    fn from(v: &str) -> Self {
        DbValue::Text(v.to_owned())
    }
}

impl From<&String> for DbValue {
    fn from(v: &String) -> Self {
        DbValue::Text(v.clone())
    }
}

impl From<Vec<u8>> for DbValue {
    fn from(v: Vec<u8>) -> Self {
        DbValue::Blob(v)
    }
}

impl<T> From<Option<T>> for DbValue
where
    DbValue: From<T>,
{
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => DbValue::from(inner),
            None => DbValue::Null,
        }
    }
}

impl DbValue {
    /// Convert to a Turso bound value (the engine's native parameter type).
    pub(crate) fn into_turso_value(self) -> turso::Value {
        match self {
            DbValue::Null => turso::Value::Null,
            DbValue::Int(i) => turso::Value::Integer(i),
            DbValue::Real(f) => turso::Value::Real(f),
            DbValue::Text(s) => turso::Value::Text(s),
            DbValue::Blob(b) => turso::Value::Blob(b),
        }
    }
}

/// Build a `Vec<DbValue>` parameter list, converting each argument with `.into()`.
///
/// ```ignore
/// db.execute("INSERT INTO t (a, b) VALUES (?, ?)", params![id, name]).await?;
/// ```
macro_rules! params {
    () => { Vec::<$crate::core::db::DbValue>::new() };
    ($($value:expr),+ $(,)?) => {
        vec![$($crate::core::db::DbValue::from($value)),+]
    };
}

pub(crate) use params;
