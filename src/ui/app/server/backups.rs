use crate::core::server::{
    BackupPruneOutcome, BackupPrunePlan, ServerBackupListing, ServerBackupOutcome,
    ServerBackupPlan, ServerBackupVerificationReport, ServerRestoreOutcome, ServerRestorePlan,
    ServerRestoreStatus,
};

#[derive(Default)]
pub(crate) struct ServerBackupViewState {
    pub live_mode: bool,
    pub keep_last_input: String,
    pub max_bytes_input: String,
    pub focus_prune_cancel: bool,
    pub plan: Option<Box<ServerBackupPlan>>,
    pub outcome: Option<Box<ServerBackupOutcome>>,
    pub listing: Option<Box<ServerBackupListing>>,
    pub selection: Option<String>,
    pub verification: Option<Box<ServerBackupVerificationReport>>,
    pub prune_plan: Option<Box<BackupPrunePlan>>,
    pub prune_outcome: Option<Box<BackupPruneOutcome>>,
    pub validation_error: Option<String>,
    pub restore_plan: Option<Box<ServerRestorePlan>>,
    pub restore_status: Option<ServerRestoreStatus>,
    pub restore_outcome: Option<Box<ServerRestoreOutcome>>,
}

impl ServerBackupViewState {
    pub fn reset_for_instance(&mut self) {
        self.focus_prune_cancel = false;
        self.plan = None;
        self.outcome = None;
        self.listing = None;
        self.selection = None;
        self.verification = None;
        self.prune_plan = None;
        self.prune_outcome = None;
        self.validation_error = None;
        self.restore_plan = None;
        self.restore_status = None;
        self.restore_outcome = None;
        self.keep_last_input.clear();
        self.max_bytes_input.clear();
    }

    pub fn sync_retention_inputs(&mut self, listing: &ServerBackupListing) {
        self.keep_last_input = listing.retention.policy.keep_last.to_string();
        self.max_bytes_input = listing
            .retention
            .policy
            .max_total_bytes
            .map_or_else(String::new, |bytes| bytes.to_string());
    }
}
