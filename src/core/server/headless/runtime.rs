use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use foxy_supervisor::control::handoff::ProtectedDirectory;

use super::plan::{HeadlessLaunchPlan, PARAMETER_FILE};
use crate::core::server::ServerStoreError;

pub(in crate::core::server) const READY_WINDOW: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum HeadlessReadiness {
    NoQueryEndpoint,
    NotResponding,
    Stale,
    Ready,
}

impl HeadlessReadiness {
    pub(in crate::core::server) fn evaluate(
        query_endpoint: bool,
        last_success: Option<Instant>,
        now: Instant,
    ) -> Self {
        match (query_endpoint, last_success) {
            (false, _) => Self::NoQueryEndpoint,
            (true, None) => Self::NotResponding,
            (true, Some(at)) if now.saturating_duration_since(at) > READY_WINDOW => Self::Stale,
            (true, Some(_)) => Self::Ready,
        }
    }

    pub(in crate::core::server) fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::NoQueryEndpoint => Some(
                "Headless clients start only after the server answers its query port, and this runtime has no query endpoint. Set a hostname and leave the Steam query port enabled.",
            ),
            Self::NotResponding => Some(
                "The server has not answered its query port yet. Wait until monitoring shows it responding, then start headless clients.",
            ),
            Self::Stale => Some(
                "The server stopped answering its query port. Headless clients were not started.",
            ),
        }
    }
}

fn private_directory(path: &Path) -> Result<ProtectedDirectory, ServerStoreError> {
    let opened = if fs::symlink_metadata(path).is_ok() {
        ProtectedDirectory::open(path)
    } else {
        ProtectedDirectory::create(path)
    };
    opened.map_err(|_| ServerStoreError::UnsafePath)
}

fn owned_directory(path: &Path) -> Result<(), ServerStoreError> {
    crate::core::server::store::check_directory_chain(path)?;
    if fs::symlink_metadata(path).is_err() {
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ServerStoreError::Io),
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ServerStoreError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ServerStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o700 {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    Ok(())
}

pub(in crate::core::server) fn parameter_bytes(secret: &str) -> Result<Vec<u8>, ServerStoreError> {
    if secret.is_empty()
        || secret.len() > 1024
        || secret.trim() != secret
        || secret.chars().any(char::is_control)
    {
        return Err(ServerStoreError::Invalid(
            "The join password cannot be handed to a headless client: it must be non-empty, at most 1024 bytes, and free of line breaks, control characters and surrounding spaces.".into(),
        ));
    }
    Ok(format!("-password={secret}\n").into_bytes())
}

pub(in crate::core::server) fn prepare_headless_state(
    state_root: &Path,
    plan: &HeadlessLaunchPlan,
    secret: Option<&str>,
) -> Result<(), ServerStoreError> {
    let headless = state_root.join("headless");
    if plan.state_root != headless.join(&plan.id)
        || plan.profiles != plan.state_root.join("home/profiles")
        || plan.join_password != secret.is_some()
    {
        return Err(ServerStoreError::UnsafePath);
    }
    owned_directory(&headless)?;
    owned_directory(&plan.state_root)?;
    for relative in [
        "home",
        "home/profiles",
        "home/config",
        "home/data",
        "home/cache",
        "home/runtime",
    ] {
        owned_directory(&plan.state_root.join(relative))?;
    }
    let private = plan.state_root.join("private");
    crate::core::server::store::check_directory_chain(&private)?;
    let directory = private_directory(&private)?;
    let existing = directory.read(PARAMETER_FILE).ok();
    match (secret, existing) {
        (Some(secret), existing) => {
            let bytes = parameter_bytes(secret)?;
            if existing.as_deref() == Some(bytes.as_slice()) {
                return Ok(());
            }
            if let Some(old) = existing {
                directory
                    .remove_exact(PARAMETER_FILE, &old)
                    .map_err(|_| ServerStoreError::Io)?;
            }
            directory
                .publish_new(PARAMETER_FILE, &bytes)
                .map_err(|_| ServerStoreError::Io)
        }
        (None, Some(old)) => directory
            .remove_exact(PARAMETER_FILE, &old)
            .map_err(|_| ServerStoreError::Io),
        (None, None) => Ok(()),
    }
}
