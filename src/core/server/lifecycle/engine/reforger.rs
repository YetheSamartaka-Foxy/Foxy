use super::*;
use crate::core::server::rcon::{RconCommandState, owned};

impl Runs {
    pub(super) fn reload_reforger_scenario(
        &mut self,
        id: &ServerInstanceId,
        expected: u64,
        timeout_ms: u32,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if !self.store.capabilities().reforger_administration {
            return Err(ServerStoreError::UnsupportedGame);
        }
        control.checkpoint()?;
        let run = self
            .runs
            .get(id.as_str())
            .ok_or(ServerStoreError::StaleRevision)?;
        if run.status.run_generation != expected || run.session.is_none() {
            return Err(ServerStoreError::StaleRevision);
        }
        let plan = run.rcon.clone().ok_or_else(|| {
            ServerStoreError::Invalid(
                "Scenario reload requires committed Reforger RCon settings.".into(),
            )
        })?;
        if plan
            .reforger
            .as_ref()
            .is_none_or(|access| !access.permits("#restart", true))
        {
            return Err(ServerStoreError::Invalid("The committed RCon permission or filters refuse scenario reload. Nothing was sent.".into()));
        }
        let scenario = run
            .reforger_scenario
            .clone()
            .ok_or(ServerStoreError::Corrupt)?;
        let before = self
            .store
            .read_monitoring_snapshot(&run.status)?
            .ok_or_else(|| {
                ServerStoreError::Invalid(
                    "Scenario reload requires a current engine observation.".into(),
                )
            })?;
        let cursor = before.logs.next_sequence;
        let original_pid = run.status.process_id;
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms.min(120_000)));
        let record = owned::command_once(
            &self.store,
            id,
            &self.scope,
            expected,
            &plan,
            &format!("{}_reload", execution.identity().operation_id),
            "#restart",
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5)),
            control,
        )?;
        let mut observed_at = None;
        let mut verified = false;
        if record.state == RconCommandState::Replied {
            while Instant::now() < deadline && !control.snapshot().cancel_requested {
                self.observe_all();
                let run = self.runs.get(id.as_str()).expect("retained run");
                if run.session.is_none() || run.status.process_id != original_pid {
                    break;
                }
                if let Some(snapshot) = self.store.read_monitoring_snapshot(&run.status)? {
                    let transitioned =
                        scenario_loaded_since(&snapshot.logs.entries, cursor, &scenario);
                    if transitioned && observed_at.is_none() {
                        observed_at = Some(crate::core::server::jobs::unix_ms());
                    }
                    let fresh_query = snapshot
                        .query
                        .last_success_unix_ms
                        .is_some_and(|time| observed_at.is_some_and(|observed| time > observed));
                    let report = crate::core::server::monitoring::InstanceMonitoring::new(
                        run.status.clone(),
                        Some(snapshot),
                        crate::core::server::jobs::unix_ms(),
                    );
                    if transitioned && fresh_query && report.query_ready {
                        verified = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let status = self
            .runs
            .get(id.as_str())
            .expect("retained run")
            .status
            .clone();
        Ok(RuntimeOperationOutcome {
            status,
            completed: verified,
            cancelled: control.snapshot().cancel_requested && !verified,
            message: Some(if verified {
                "The same owned process loaded the committed scenario and answered a fresh query. This was a scenario reload.".into()
            } else {
                "Scenario reload was sent at most once. Its completed effect is unconfirmed; it will not be repeated automatically. Review the current run and RCon history.".into()
            }),
            backup_id: None,
            backup_consistency: None,
        })
    }
}

fn scenario_loaded_since(
    entries: &[crate::core::server::LogLine],
    cursor: u64,
    scenario: &str,
) -> bool {
    let mut command_stream = None;
    let mut start = None;
    let mut loaded = false;
    for line in entries.iter().filter(|line| line.sequence >= cursor) {
        if line.stream != "stdout" {
            continue;
        }
        let lower = line.text.to_ascii_lowercase();
        if lower.contains("version ") && lower.contains(" built ") {
            return false;
        }
        if line.text.contains("[RCON] Process Command: #restart") {
            command_stream = Some(line.stream.as_str());
            start = None;
            loaded = false;
        }
        if command_stream != Some(line.stream.as_str()) {
            continue;
        }
        if line
            .text
            .contains("[SaveGameManager] Starting new playthrough")
            && line.text.contains(&format!("for mission '{scenario}'"))
        {
            start = Some(line.sequence);
        }
        if start.is_some_and(|sequence| sequence < line.sequence)
            && line.text.contains("WORLD")
            && line.text.contains("Entities load '")
        {
            loaded = true;
        }
    }
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::LogLine;

    fn line(sequence: u64, text: &str) -> LogLine {
        LogLine {
            sequence,
            stream: "stdout".into(),
            text: text.into(),
        }
    }

    #[test]
    fn reload_requires_fresh_committed_scenario_before_world_load() {
        let scenario = "{0123456789ABCDEF}Missions/Example.conf";
        let entries = [
            line(10, "BACKEND : [RCON] Process Command: #restart"),
            line(
                11,
                &format!(
                    "DEFAULT : [SaveGameManager] Starting new playthrough nr.1 '' for mission '{scenario}'."
                ),
            ),
            line(12, " WORLD : Entities load '$Game:worlds/Example.ent'"),
        ];
        assert!(scenario_loaded_since(&entries, 10, scenario));
        assert!(!scenario_loaded_since(&entries, 11, scenario));
        assert!(!scenario_loaded_since(
            &entries,
            10,
            "{FEDCBA9876543210}Missions/Other.conf"
        ));
        assert!(!scenario_loaded_since(&entries[..1], 10, scenario));
        assert!(!scenario_loaded_since(
            &[entries[0].clone(), entries[2].clone(), entries[1].clone()],
            10,
            scenario
        ));
        let mut relaunched = entries.to_vec();
        relaunched.push(line(13, "ENGINE : Version 1.9.0.37 built synthetic"));
        assert!(!scenario_loaded_since(&relaunched, 10, scenario));
        let mut wrong_stream = entries.clone();
        wrong_stream[2].stream = "old_engine_log".into();
        assert!(!scenario_loaded_since(&wrong_stream, 10, scenario));
        let mut duplicate_stream = entries.to_vec();
        let mut duplicate = line(13, "BACKEND : [RCON] Process Command: #restart");
        duplicate.stream = "engine_1".into();
        duplicate_stream.push(duplicate);
        assert!(scenario_loaded_since(&duplicate_stream, 10, scenario));
    }
}
