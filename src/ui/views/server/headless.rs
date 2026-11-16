use crate::core::server::{
    HeadlessClientState, HeadlessClientStatus, HeadlessConnection, HeadlessReason,
    InstanceRuntimeStatus, ServerInstanceId,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

fn state_text(client: &HeadlessClientStatus) -> String {
    let state = match client.state {
        HeadlessClientState::WaitingForServer => {
            tr("Waiting for the server to answer its query port")
        }
        HeadlessClientState::Scheduled => tr("Start scheduled"),
        HeadlessClientState::Running => tr("Running"),
        HeadlessClientState::Stopped => tr("Stopped"),
        HeadlessClientState::RestartSuspended => tr("Automatic restarts suspended"),
    };
    match client.reason {
        Some(reason) => format!("{state}. {}", reason_text(reason)),
        None => state,
    }
}

fn reason_text(reason: HeadlessReason) -> String {
    match reason {
        HeadlessReason::ManualStartRequired => tr("Starts only when started explicitly."),
        HeadlessReason::StoppedByOperator => {
            tr("Stopped by an operator; it will not restart on its own.")
        }
        HeadlessReason::RestartDisabled => tr("It exited and automatic restart is off."),
        HeadlessReason::RestartBudgetExhausted => {
            tr("It exited too often. Start it explicitly to retry.")
        }
        HeadlessReason::StartFailed => {
            tr("Its start failed. Inspect installation consumers, then start it explicitly.")
        }
        HeadlessReason::ServerNotRunning => tr("The server run is not running."),
        HeadlessReason::PausedByServerStop => tr(
            "Automatic starts are paused because a server stop did not finish. Start it explicitly, or stop the server.",
        ),
    }
}

fn connection_text(client: &HeadlessClientStatus, now: u64) -> Option<String> {
    Some(match client.connection? {
        HeadlessConnection::Pending => tr("Join pending: no started mission in its log yet."),
        HeadlessConnection::Joined => tr_fmt(
            "Joined {seconds} s ago: its log reports a started mission.",
            &[(
                "seconds",
                client
                    .joined_unix_ms
                    .map_or(0, |time| now.saturating_sub(time) / 1000)
                    .to_string(),
            )],
        ),
        HeadlessConnection::NoEvidence => tr(
            "No join evidence within the join timeout. Check the mission's headless slots and the join password.",
        ),
        HeadlessConnection::Unreadable => tr("Join evidence is unreadable."),
    })
}

impl Foxy {
    pub(super) fn render_headless_lifecycle(
        &mut self,
        ui: &mut Ui,
        id: &ServerInstanceId,
        status: &InstanceRuntimeStatus,
        busy: bool,
        dirty: bool,
    ) {
        let monitoring = self
            .server_view
            .lifecycle_monitoring
            .as_ref()
            .filter(|monitoring| {
                &monitoring.status.instance_id == id
                    && monitoring.status.run_generation == status.run_generation
            });
        let query_ready = monitoring.is_some_and(|monitoring| monitoring.query_ready);
        let group = monitoring
            .and_then(|monitoring| monitoring.observation.as_ref())
            .and_then(|observation| observation.headless.clone());
        let enabled = !busy && !dirty;
        let expected_run_generation = status.run_generation;
        let allow_force = self.server_view.lifecycle_allow_force;
        ui.label(tr_fmt(
            "Headless client processes running: {count}",
            &[("count", status.headless_running.to_string())],
        ));
        if !query_ready {
            ui.label(tr(
                "Headless clients start after this run answers its query port.",
            ));
        }
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    enabled && query_ready,
                    egui::Button::new(tr("Start headless clients")),
                )
                .clicked()
            {
                action = Some(ServerTaskAction::StartHeadlessClients {
                    id: id.clone(),
                    expected_run_generation,
                    client: None,
                });
            }
            if ui
                .add_enabled(enabled, egui::Button::new(tr("Stop headless clients")))
                .clicked()
            {
                action = Some(ServerTaskAction::StopHeadlessClients {
                    id: id.clone(),
                    expected_run_generation,
                    client: None,
                    force: false,
                });
            }
            if allow_force
                && ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(tr("Force stop headless clients")),
                    )
                    .clicked()
            {
                action = Some(ServerTaskAction::StopHeadlessClients {
                    id: id.clone(),
                    expected_run_generation,
                    client: None,
                    force: true,
                });
            }
        });
        let now = unix_ms();
        for client in group.iter().flat_map(|group| &group.clients) {
            ui.group(|ui| {
                ui.label(egui::RichText::new(format!("{} ({})", client.name, client.id)).strong());
                ui.label(state_text(client));
                if let Some(connection) = connection_text(client, now) {
                    ui.label(connection);
                }
                ui.label(tr_fmt(
                    "Starts: {starts}; automatic restarts in the last 10 minutes: {restarts}",
                    &[
                        ("starts", client.starts.to_string()),
                        ("restarts", client.automatic_restarts.to_string()),
                    ],
                ));
                if let Some(next) = client.next_start_unix_ms {
                    ui.label(tr_fmt(
                        "Next start in {seconds} s",
                        &[(
                            "seconds",
                            next.saturating_sub(now).div_ceil(1000).to_string(),
                        )],
                    ));
                }
                if let Some(exit) = &client.last_exit {
                    ui.label(match (exit.exit_code, exit.requested) {
                        (Some(code), true) => tr_fmt(
                            "Last exit: code {code}, requested by Foxy",
                            &[("code", code.to_string())],
                        ),
                        (Some(code), false) => tr_fmt(
                            "Last exit: code {code}, unexpected",
                            &[("code", code.to_string())],
                        ),
                        (None, true) => tr("Last exit: no code, requested by Foxy"),
                        (None, false) => tr("Last exit: no code, unexpected"),
                    });
                }
                let running = client.state == HeadlessClientState::Running;
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            enabled && query_ready && !running,
                            egui::Button::new(tr_fmt(
                                "Start {name}",
                                &[("name", client.name.clone())],
                            )),
                        )
                        .clicked()
                    {
                        action = Some(ServerTaskAction::StartHeadlessClients {
                            id: id.clone(),
                            expected_run_generation,
                            client: Some(client.id.clone()),
                        });
                    }
                    if ui
                        .add_enabled(
                            enabled && (running || client.wanted),
                            egui::Button::new(tr_fmt(
                                "Stop {name}",
                                &[("name", client.name.clone())],
                            )),
                        )
                        .clicked()
                    {
                        action = Some(ServerTaskAction::StopHeadlessClients {
                            id: id.clone(),
                            expected_run_generation,
                            client: Some(client.id.clone()),
                            force: false,
                        });
                    }
                    if ui
                        .add_enabled(
                            enabled && query_ready && running,
                            egui::Button::new(tr_fmt(
                                "Restart {name}",
                                &[("name", client.name.clone())],
                            )),
                        )
                        .clicked()
                    {
                        action = Some(ServerTaskAction::RestartHeadlessClients {
                            id: id.clone(),
                            expected_run_generation,
                            client: Some(client.id.clone()),
                        });
                    }
                });
            });
        }
        if group.is_none() && status.headless_running == 0 {
            ui.label(tr(
                "Per-client status appears once the owner reports this run's observation.",
            ));
        }
        ui.label(tr("Joined means the client's own log reports a started mission. A later disconnect of a live process is not detected, and mission slots and AI offload are not verified."));
        if let Some(action) = action {
            self.start_server_task(action);
        }
    }
}
