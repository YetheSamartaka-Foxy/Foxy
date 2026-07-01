use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct DirectDownloadSession {
    pub source_url: String,
    pub destination_folder: String,
    pub target_label: String,
    pub files_total: usize,
    pub files_done: usize,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub finished_at: Option<Instant>,
    pub error_message: Option<String>,
}

impl DirectDownloadSession {
    pub fn is_running(&self) -> bool {
        self.finished_at.is_none()
    }

    pub fn finished_successfully(&self) -> bool {
        self.finished_at.is_some() && self.error_message.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct DirectDownloadTarget {
    pub remote_url: String,
    pub local_path: PathBuf,
    pub size_bytes: u64,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct DirectDownloadPlan {
    pub target_label: String,
    pub files: Vec<DirectDownloadTarget>,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub enum DirectDownloadProgressEvent {
    PlanResolved {
        target_label: String,
        files_total: usize,
        total_bytes: u64,
    },
    Progress {
        label: String,
        percent: f32,
        files_done: usize,
        files_total: usize,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Finished {
        error_message: Option<String>,
        files_done: usize,
        files_total: usize,
        downloaded_bytes: u64,
        total_bytes: u64,
        elapsed: Duration,
    },
}
