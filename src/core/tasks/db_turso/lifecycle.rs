use std::path::{Path, PathBuf};
use std::sync::Arc;

use turso::Database;

use super::{database_file_path, drain_connection_pool, open_and_prepare_database};

enum Admission {
    Initial,
    Claimed(PathBuf),
    Suspended,
}

struct DatabaseSlot {
    entry: Option<(PathBuf, Arc<Database>)>,
    admission: Admission,
}

impl DatabaseSlot {
    const fn new() -> Self {
        Self {
            entry: None,
            admission: Admission::Initial,
        }
    }

    fn validate_admission(&self, path: &Path) -> Result<(), String> {
        match &self.admission {
            Admission::Suspended => return Err("Database access is suspended after close; an explicit ownership claim must resume it".to_string()),
            Admission::Claimed(expected) if expected != path => return Err("Database request belongs to a different game space".to_string()),
            _ => {}
        }
        if self
            .entry
            .as_ref()
            .is_some_and(|(open_path, _)| open_path != path)
        {
            return Err("Close the current database before opening another game space".to_string());
        }
        Ok(())
    }

    fn resume(&mut self, path: &Path) -> Result<(), String> {
        if self
            .entry
            .as_ref()
            .is_some_and(|(open_path, _)| open_path != path)
        {
            return Err("The previous game space's database is still open".to_string());
        }
        self.admission = Admission::Claimed(path.to_path_buf());
        Ok(())
    }

    fn try_close(&mut self) -> Result<Option<PathBuf>, String> {
        let Some((path, database)) = self.entry.take() else {
            let path = match &self.admission {
                Admission::Claimed(path) => Some(path.clone()),
                _ => None,
            };
            self.admission = Admission::Suspended;
            return Ok(path);
        };
        drain_connection_pool(&database);
        match Arc::try_unwrap(database) {
            Ok(database) => {
                drop(database);
                self.admission = Admission::Suspended;
                Ok(Some(path))
            }
            Err(database) => {
                self.entry = Some((path, database));
                Err("Database tasks still hold this game space open; wait for them to finish and retry".to_string())
            }
        }
    }
}

static DB_SLOT: tokio::sync::Mutex<DatabaseSlot> =
    tokio::sync::Mutex::const_new(DatabaseSlot::new());

pub(crate) async fn init_turso_database() -> Arc<Database> {
    init_turso_database_with_path().await.1
}

pub(crate) async fn init_turso_database_with_path() -> (PathBuf, Arc<Database>) {
    let path = database_file_path();
    let mut slot = DB_SLOT.lock().await;
    slot.validate_admission(&path)
        .unwrap_or_else(|error| panic!("{error}"));
    if let Some((_, database)) = &slot.entry {
        assert!(
            crate::core::tasks::db_process_lock::holds(&path),
            "Database ownership was lost"
        );
        return (path, database.clone());
    }
    let database = open_and_prepare_database(&path).await;
    slot.admission = Admission::Claimed(path.clone());
    slot.entry = Some((path.clone(), database.clone()));
    (path, database)
}

pub(crate) async fn resume_database_access(path: &Path) -> Result<(), String> {
    let mut slot = DB_SLOT.lock().await;
    if !crate::core::tasks::db_process_lock::holds(path) {
        return Err(
            "Cannot resume database access without an exclusive ownership claim".to_string(),
        );
    }
    slot.resume(path)
}

pub(crate) async fn close_active_database() -> Result<(), String> {
    let _exclusive = crate::core::tasks::init_database::DB_EXCLUSIVE
        .clone()
        .try_write_owned()
        .map_err(|_| "Database access is still in progress; retry when it finishes".to_string())?;
    let mut slot = DB_SLOT.try_lock().map_err(|_| {
        "Database initialization is still in progress; retry when it finishes".to_string()
    })?;
    if let Some((_, database)) = &slot.entry {
        crate::core::tasks::create_context::invalidate_shared_background_context(database)?;
    }
    if let Some(path) = slot.try_close()? {
        crate::core::tasks::db_process_lock::release_for(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_close_cycle_with_a_retained_background_context() {
        if std::env::var_os("FOXY_TEST_DB_CLOSE_PROBE").is_none() {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "core::tasks::db_turso::lifecycle::tests::ownership_close_cycle_with_a_retained_background_context", "--nocapture"])
                .env("FOXY_TEST_DB_CLOSE_PROBE", "1")
                .env("FOXY_CONFIG_DIR", root.path())
                .output().unwrap();
            assert!(
                output.status.success(),
                "close probe failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use crate::core::tasks::db_process_lock::{self, LockOutcome};
        crate::core::game::spaces::prepare_game_spaces_layout().unwrap();
        let path = database_file_path();
        assert_eq!(db_process_lock::acquire(&path), LockOutcome::Acquired);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            resume_database_access(&path).await.unwrap();
            let context = crate::core::tasks::create_context::shared_background_context().await;
            assert!(close_active_database().await.is_err());
            assert!(db_process_lock::holds(&path));
            drop(context);
            close_active_database().await.unwrap();
            assert!(!db_process_lock::holds(&path));
            assert!(DB_SLOT.lock().await.validate_admission(&path).is_err());
            assert_eq!(db_process_lock::acquire(&path), LockOutcome::Acquired);
            resume_database_access(&path).await.unwrap();
            let database = init_turso_database().await;
            drop(database);
            close_active_database().await.unwrap();
            assert!(!db_process_lock::holds(&path));
        });
    }

    #[tokio::test]
    async fn retained_handles_refuse_close_and_preserve_the_slot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database.db");
        let database = Arc::new(
            super::super::build_and_bootstrap(path.to_str().unwrap())
                .await
                .unwrap(),
        );
        let mut slot = DatabaseSlot::new();
        slot.resume(&path).unwrap();
        slot.entry = Some((path.clone(), database.clone()));
        assert!(slot.try_close().is_err());
        assert!(Arc::ptr_eq(&slot.entry.as_ref().unwrap().1, &database));
        assert!(slot.validate_admission(&path).is_ok());
        drop(database);
        assert_eq!(slot.try_close().unwrap(), Some(path.clone()));
        assert!(slot.entry.is_none());
        assert!(slot.validate_admission(&path).is_err());
        slot.resume(&path).unwrap();
        assert!(slot.validate_admission(&path).is_ok());
    }

    #[tokio::test]
    async fn checked_out_connection_keeps_the_database_owned() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database.db");
        let database = Arc::new(
            super::super::build_and_bootstrap(path.to_str().unwrap())
                .await
                .unwrap(),
        );
        let connection = super::super::connect_pooled(&database).await.unwrap();
        let mut slot = DatabaseSlot::new();
        slot.resume(&path).unwrap();
        slot.entry = Some((path.clone(), database));
        assert!(slot.try_close().is_err());
        drop(connection);
        assert_eq!(slot.try_close().unwrap(), Some(path));
    }

    #[test]
    fn admission_requires_explicit_resume_and_rejects_a_stale_space() {
        let first = Path::new("games/alpha/database.db");
        let second = Path::new("games/beta/database.db");
        let mut slot = DatabaseSlot::new();
        slot.resume(first).unwrap();
        assert!(slot.validate_admission(second).is_err());
        assert_eq!(slot.try_close().unwrap(), Some(first.to_path_buf()));
        assert!(slot.validate_admission(first).is_err());
        assert!(slot.validate_admission(second).is_err());
        slot.resume(second).unwrap();
        assert!(slot.validate_admission(first).is_err());
        assert!(slot.validate_admission(second).is_ok());
    }

    #[tokio::test]
    async fn active_seam_access_prevents_close_before_any_ownership_change() {
        let _reader = crate::core::tasks::init_database::acquire_db_shared().await;
        let error = close_active_database().await.unwrap_err();
        assert!(error.contains("still in progress"));
    }
}
