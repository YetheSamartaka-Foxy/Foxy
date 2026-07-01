//! Storage-neutral result row for the DB seam (plan.md §5.1).
//!
//! A [`DbRow`] is what `query_all`/`query_one` hand back. It exposes
//! column-name accessors (`get_i64`, `get_string`, …) over a materialized Turso
//! row (parallel column-name / value vectors). Call sites read columns by name
//! via `row.get_string("col")` rather than positional indexing.

use std::sync::Arc;

use super::DbErr;
use super::value::DbValue;

/// One materialized result row: parallel column-name / value vectors.
pub(crate) struct DbRow {
    pub(crate) columns: Arc<Vec<String>>,
    pub(crate) values: Vec<DbValue>,
}

impl DbRow {
    fn value(&self, col: &str) -> Result<DbValue, DbErr> {
        let idx = self
            .columns
            .iter()
            .position(|c| c == col)
            .ok_or_else(|| DbErr::Custom(format!("column '{col}' not found in result set")))?;
        self.values
            .get(idx)
            .cloned()
            .ok_or_else(|| DbErr::Custom(format!("column '{col}' index out of bounds")))
    }

    pub(crate) fn get_i64(&self, col: &str) -> Result<i64, DbErr> {
        match self.value(col)? {
            DbValue::Int(i) => Ok(i),
            DbValue::Real(f) => Ok(f as i64),
            other => Err(DbErr::Custom(format!(
                "column '{col}' is {other:?}, expected INTEGER"
            ))),
        }
    }

    pub(crate) fn get_opt_i64(&self, col: &str) -> Result<Option<i64>, DbErr> {
        match self.value(col)? {
            DbValue::Null => Ok(None),
            DbValue::Int(i) => Ok(Some(i)),
            DbValue::Real(f) => Ok(Some(f as i64)),
            other => Err(DbErr::Custom(format!(
                "column '{col}' is {other:?}, expected INTEGER or NULL"
            ))),
        }
    }

    pub(crate) fn get_string(&self, col: &str) -> Result<String, DbErr> {
        match self.value(col)? {
            DbValue::Text(s) => Ok(s),
            other => Err(DbErr::Custom(format!(
                "column '{col}' is {other:?}, expected TEXT"
            ))),
        }
    }

    pub(crate) fn get_opt_string(&self, col: &str) -> Result<Option<String>, DbErr> {
        match self.value(col)? {
            DbValue::Null => Ok(None),
            DbValue::Text(s) => Ok(Some(s)),
            other => Err(DbErr::Custom(format!(
                "column '{col}' is {other:?}, expected TEXT or NULL"
            ))),
        }
    }

    pub(crate) fn get_f64(&self, col: &str) -> Result<f64, DbErr> {
        match self.value(col)? {
            DbValue::Real(f) => Ok(f),
            DbValue::Int(i) => Ok(i as f64),
            other => Err(DbErr::Custom(format!(
                "column '{col}' is {other:?}, expected REAL"
            ))),
        }
    }

    /// Read an INTEGER column as a boolean (`!= 0`), matching SQLite's storage of
    /// booleans as integers.
    pub(crate) fn get_bool(&self, col: &str) -> Result<bool, DbErr> {
        Ok(self.get_i64(col)? != 0)
    }
}
