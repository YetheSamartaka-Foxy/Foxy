use super::*;
use crate::core::server::jobs::PlayerPolicy;
use crate::core::server::rcon::arma3::{ArmaAdminCommand, parse_missions};
use crate::core::server::rcon::{RconCommandState, owned};

impl Runs {
    pub(super) fn announce_maintenance(
        &mut self,
        id: &ServerInstanceId,
        expected: u64,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        let Some((occurrence, policy)) = &execution.arma_maintenance else {
            return Ok(());
        };
        if !self.store.capabilities().arma_administration || policy.require_backup {
            return Err(ServerStoreError::Invalid(
                "The required Arma maintenance adapter is unavailable.".into(),
            ));
        }
        let Some(message) = &policy.announcement else {
            return Ok(());
        };
        let run = self
            .runs
            .get(id.as_str())
            .ok_or(ServerStoreError::StaleRevision)?;
        if run.status.run_generation != expected || run.session.is_none() {
            return Err(ServerStoreError::StaleRevision);
        }
        let plan = run.rcon.as_ref().ok_or_else(|| {
            ServerStoreError::Invalid("The announcement requires committed RCon settings.".into())
        })?;
        let text = ArmaAdminCommand::Announce {
            message: message.clone(),
        }
        .wire_text()
        .map_err(ServerStoreError::Invalid)?;
        let record = owned::command_once(
            &self.store,
            id,
            &self.scope,
            expected,
            plan,
            &format!("{occurrence}_announce"),
            &text,
            Duration::from_secs(5),
            control,
        )?;
        if record.state != RconCommandState::Replied {
            return Err(ServerStoreError::Invalid("The announcement outcome is unconfirmed. Maintenance did not continue and the announcement will not be repeated automatically.".into()));
        }
        let sent = record
            .finished_unix_ms
            .and_then(|time| u64::try_from(time).ok())
            .unwrap_or_default();
        let remaining = sent
            .saturating_add(u64::from(policy.announcement_delay_seconds) * 1000)
            .saturating_sub(crate::core::server::jobs::unix_ms())
            .min(u64::from(policy.announcement_delay_seconds) * 1000);
        let deadline = Instant::now() + Duration::from_millis(remaining);
        while Instant::now() < deadline {
            control.checkpoint()?;
            self.observe_all();
            if !self
                .runs
                .get(id.as_str())
                .is_some_and(|run| run.session.is_some() && run.status.run_generation == expected)
            {
                return Err(ServerStoreError::Invalid("The server stopped during the announcement delay; maintenance did not restart it.".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        control.checkpoint()?;
        if execution.maintenance_player_policy == Some(PlayerPolicy::RequireEmpty) {
            let run = self
                .runs
                .get(id.as_str())
                .ok_or(ServerStoreError::StaleRevision)?;
            let snapshot = self.store.read_monitoring_snapshot(&run.status)?;
            let report = crate::core::server::monitoring::InstanceMonitoring::new(
                run.status.clone(),
                snapshot,
                crate::core::server::jobs::unix_ms(),
            );
            if !report.query_ready
                || report
                    .observation
                    .and_then(|snapshot| snapshot.query.last_good)
                    .is_none_or(|summary| summary.players != 0)
            {
                return Err(ServerStoreError::Invalid("The empty-server condition changed during the announcement delay; maintenance did not continue.".into()));
            }
        }
        Ok(())
    }

    pub(super) fn rotate_mission(
        &mut self,
        id: &ServerInstanceId,
        expected: u64,
        template: &str,
        timeout_ms: u32,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if !self.store.capabilities().arma_administration {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let command = ArmaAdminCommand::ChangeMission {
            template: template.into(),
        };
        let text = command.wire_text().map_err(ServerStoreError::Invalid)?;
        let run = self
            .runs
            .get(id.as_str())
            .ok_or(ServerStoreError::StaleRevision)?;
        if run.status.run_generation != expected || run.session.is_none() {
            return Err(ServerStoreError::StaleRevision);
        }
        let plan = run.rcon.clone().ok_or_else(|| {
            ServerStoreError::Invalid("Mission rotation requires committed RCon settings.".into())
        })?;
        let status = run.status.clone();
        let identity = execution.arma_maintenance.as_ref().map_or_else(
            || execution.identity().operation_id.clone(),
            |(occurrence, _)| occurrence.clone(),
        );
        let listed = owned::command_once(
            &self.store,
            id,
            &self.scope,
            expected,
            &plan,
            &format!("{identity}_missions"),
            "missions",
            Duration::from_secs(5),
            control,
        )?;
        let missions = listed.response.as_deref().map(parse_missions);
        if listed.state != RconCommandState::Replied
            || !missions.is_some_and(|missions| {
                missions.complete
                    && missions.missions.iter().any(|mission| {
                        mission
                            .trim_end_matches(".pbo")
                            .eq_ignore_ascii_case(template)
                    })
            })
        {
            return Err(ServerStoreError::Invalid("The selected mission could not be verified in the deployed mission list. Nothing was changed.".into()));
        }
        self.announce_maintenance(id, expected, execution, control)?;
        control.checkpoint()?;
        let before = self
            .store
            .read_monitoring_snapshot(&status)?
            .ok_or_else(|| {
                ServerStoreError::Invalid(
                    "Mission rotation requires a current engine observation.".into(),
                )
            })?;
        let before_sequence = before.logs.next_sequence;
        // Arma can leave the initial mission id blank; the transition still needs a fresh full id.
        let previous_mission_id = latest_mission_id(&before.logs.entries).unwrap_or_default();
        let record = owned::command_once(
            &self.store,
            id,
            &self.scope,
            expected,
            &plan,
            &format!("{identity}_rotate"),
            &text,
            Duration::from_secs(5),
            control,
        )?;
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms.min(120_000)));
        let mut verified = false;
        let mut transition_observed_at = None;
        if record.state == RconCommandState::Replied {
            while Instant::now() < deadline && !control.snapshot().cancel_requested {
                self.observe_all();
                let run = self.runs.get(id.as_str()).expect("retained run");
                if run.session.is_none() {
                    break;
                }
                if let Some(snapshot) = self.store.read_monitoring_snapshot(&run.status)? {
                    let fresh_read = mission_started_since(
                        &snapshot.logs.entries,
                        before_sequence,
                        &previous_mission_id,
                        template,
                    );
                    if fresh_read && transition_observed_at.is_none() {
                        transition_observed_at = Some(crate::core::server::jobs::unix_ms());
                    }
                    let fresh_query = snapshot.query.last_success_unix_ms.is_some_and(|query| {
                        transition_observed_at.is_some_and(|observed| query > observed)
                    });
                    let report = crate::core::server::monitoring::InstanceMonitoring::new(
                        run.status.clone(),
                        Some(snapshot),
                        crate::core::server::jobs::unix_ms(),
                    );
                    if fresh_read && fresh_query && report.query_ready {
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
                "The same owned server read the selected mission and answered a fresh query. Its process was not restarted.".into()
            } else {
                "Mission rotation was sent at most once. Its completed effect is unconfirmed; review the server and RCon history before another action.".into()
            }),
        })
    }
}

fn latest_mission_id(entries: &[crate::core::server::LogLine]) -> Option<String> {
    entries.iter().rev().find_map(|line| {
        let (_, id) = line.text.split_once("Mission id:")?;
        let id = id.trim();
        (id.len() == 40 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| id.to_owned())
    })
}

fn mission_started_since(
    entries: &[crate::core::server::LogLine],
    sequence: u64,
    previous: &str,
    template: &str,
) -> bool {
    let Some((file, world)) = template.rsplit_once('.') else {
        return false;
    };
    let mut file_sequence = None;
    let mut world_sequence = None;
    for line in entries.iter().filter(|line| line.sequence >= sequence) {
        if let Some((_, value)) = line.text.split_once("Mission file:") {
            file_sequence = value
                .split_whitespace()
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case(file))
                .then_some(line.sequence);
            world_sequence = None;
        } else if let Some((_, value)) = line.text.split_once("Mission world:") {
            world_sequence = (file_sequence.is_some_and(|file| file < line.sequence)
                && value.trim().eq_ignore_ascii_case(world))
            .then_some(line.sequence);
        } else if let Some((_, id)) = line.text.split_once("Mission id:") {
            let id = id.trim();
            if world_sequence.is_some_and(|world| world < line.sequence)
                && id.len() == 40
                && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !id.eq_ignore_ascii_case(previous)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::LogLine;

    fn line(sequence: u64, text: &str) -> LogLine {
        LogLine {
            sequence,
            stream: "engine_0".into(),
            text: text.into(),
        }
    }

    #[test]
    fn rotation_requires_a_fresh_ordered_matching_mission_with_a_new_native_identity() {
        let old = "a".repeat(40);
        let entries = vec![
            line(20, "09:00:00 Starting mission:"),
            line(21, "09:00:00 Mission file: mission_alpha (__cur_mp)"),
            line(22, "09:00:00 Mission world: vr"),
            line(23, &format!("09:00:00 Mission id: {}", "b".repeat(40))),
        ];
        assert!(mission_started_since(
            &entries,
            20,
            &old,
            "mission_alpha.vr"
        ));
        assert!(!mission_started_since(
            &entries,
            24,
            &old,
            "mission_alpha.vr"
        ));
        assert!(!mission_started_since(
            &entries,
            20,
            &"b".repeat(40),
            "mission_alpha.vr"
        ));
        assert!(!mission_started_since(
            &entries,
            20,
            &old,
            "mission_beta.vr"
        ));
        assert!(!mission_started_since(
            &entries[2..],
            20,
            &old,
            "mission_alpha.vr"
        ));
        assert_eq!(
            latest_mission_id(&entries).as_deref(),
            Some("b".repeat(40).as_str())
        );
    }
}
