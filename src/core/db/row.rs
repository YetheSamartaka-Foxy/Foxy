use std::sync::Arc;

use super::DbErr;
use super::value::DbValue;

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

    pub(crate) fn get_bool(&self, col: &str) -> Result<bool, DbErr> {
        Ok(self.get_i64(col)? != 0)
    }
}
