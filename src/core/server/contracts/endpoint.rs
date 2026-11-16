#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

const MAX_SOCKET_PATH_UTF8: usize = foxy_supervisor::control::transport::MAX_UNIX_SOCKET_PATH_BYTES;
const ENDPOINT_KEY_HEX: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointPlacement {
    pub socket_path: PathBuf,
    pub discovery_path: PathBuf,
    pub key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointError {
    EmptyRuntime,
    EmptySpace,
    InvalidSpace,
    EmptyConfigRoot,
    UnnormalizedRoot,
    RelativeRuntime,
    RelativeSpace,
    RelativeLock,
    NestedInSpace,
    OverlapsLock,
    PathTooLong,
    PublicListener,
    UnknownKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointKind {
    UnixSocket,
    NamedPipe,
}

pub(crate) struct EndpointInputs<'a> {
    pub runtime_dir: &'a Path,
    pub config_root: &'a str,
    pub space_id: &'a str,
    pub space_dir: &'a Path,
    pub lock_registry: &'a Path,
}

pub(crate) fn normalize_config_root(raw: &str) -> Result<String, EndpointError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::EmptyConfigRoot);
    }
    if trimmed.starts_with("//") || trimmed.starts_with(r"\\") {
        return Err(EndpointError::UnnormalizedRoot);
    }
    let unified = trimmed.replace('\\', "/");
    let absolute = unified.starts_with('/');
    let mut parts = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(EndpointError::UnnormalizedRoot);
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(EndpointError::EmptyConfigRoot);
    }
    let mut normalized = String::new();
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&parts.join("/"));
    Ok(normalized)
}

pub(crate) fn endpoint_key(config_root: &str, space_id: &str) -> Result<String, EndpointError> {
    let normalized = normalize_config_root(config_root)?;
    if space_id.is_empty() {
        return Err(EndpointError::EmptySpace);
    }
    if space_id.contains('\0') {
        return Err(EndpointError::InvalidSpace);
    }
    let mut material = String::new();
    material.push_str(&normalized);
    material.push('\n');
    material.push_str(space_id);
    let hex = blake3::hash(material.as_bytes()).to_hex();
    Ok(hex[..ENDPOINT_KEY_HEX].to_string())
}

pub(crate) fn place_runtime_endpoint(
    inputs: &EndpointInputs<'_>,
) -> Result<EndpointPlacement, EndpointError> {
    if inputs.runtime_dir.as_os_str().is_empty() {
        return Err(EndpointError::EmptyRuntime);
    }
    if !inputs.runtime_dir.is_absolute() {
        return Err(EndpointError::RelativeRuntime);
    }
    if inputs.space_dir.as_os_str().is_empty() || !inputs.space_dir.is_absolute() {
        return Err(EndpointError::RelativeSpace);
    }
    if inputs.lock_registry.as_os_str().is_empty() || !inputs.lock_registry.is_absolute() {
        return Err(EndpointError::RelativeLock);
    }
    let key = endpoint_key(inputs.config_root, inputs.space_id)?;
    let socket_path = inputs.runtime_dir.join("foxy").join(format!("{key}.sock"));
    let discovery_path = inputs
        .space_dir
        .join("server")
        .join("ipc")
        .join(format!("{key}.json"));
    if socket_path.starts_with(inputs.space_dir) || inputs.space_dir.starts_with(&socket_path) {
        return Err(EndpointError::NestedInSpace);
    }
    if socket_path.starts_with(inputs.lock_registry)
        || inputs.lock_registry.starts_with(&socket_path)
    {
        return Err(EndpointError::OverlapsLock);
    }
    let Some(socket_text) = socket_path.to_str() else {
        return Err(EndpointError::PathTooLong);
    };
    if socket_text.len() > MAX_SOCKET_PATH_UTF8 {
        return Err(EndpointError::PathTooLong);
    }
    Ok(EndpointPlacement {
        socket_path,
        discovery_path,
        key,
    })
}

pub(crate) fn endpoint_kind(label: &str) -> Result<EndpointKind, EndpointError> {
    match label {
        "unix" => Ok(EndpointKind::UnixSocket),
        "pipe" => Ok(EndpointKind::NamedPipe),
        "tcp" | "udp" => Err(EndpointError::PublicListener),
        _ => Err(EndpointError::UnknownKind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(parts: &[&str]) -> PathBuf {
        let mut path = if cfg!(windows) {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        };
        for part in parts {
            path.push(part);
        }
        path
    }

    fn inputs<'a>(
        runtime_dir: &'a Path,
        space_dir: &'a Path,
        lock_registry: &'a Path,
        space_id: &'a str,
        config_root: &'a str,
    ) -> EndpointInputs<'a> {
        EndpointInputs {
            runtime_dir,
            config_root,
            space_id,
            space_dir,
            lock_registry,
        }
    }

    #[test]
    fn a_short_runtime_endpoint_is_stable_and_outside_the_space() {
        let runtime = abs(&["rt"]);
        let space = abs(&["games", "alpha"]);
        let locks = abs(&["locks"]);
        let first =
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", r"C:\Foxy")).unwrap();
        let second =
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", "C:/Foxy/")).unwrap();
        assert_eq!(first, second);
        assert!(first.socket_path.starts_with(&runtime));
        assert!(!first.socket_path.starts_with(&space));
    }

    #[test]
    fn empty_endpoint_inputs_are_rejected() {
        let runtime = abs(&["rt"]);
        let space = abs(&["games", "alpha"]);
        let locks = abs(&["locks"]);
        assert_eq!(
            place_runtime_endpoint(&inputs(&PathBuf::new(), &space, &locks, "alpha", "C:/Foxy")),
            Err(EndpointError::EmptyRuntime)
        );
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "", "C:/Foxy")),
            Err(EndpointError::EmptySpace)
        );
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha\0", "C:/Foxy")),
            Err(EndpointError::InvalidSpace)
        );
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", "   ")),
            Err(EndpointError::EmptyConfigRoot)
        );
    }

    #[test]
    fn a_relative_runtime_directory_is_rejected() {
        let space = abs(&["games", "alpha"]);
        let locks = abs(&["locks"]);
        assert_eq!(
            place_runtime_endpoint(&inputs(Path::new("rt"), &space, &locks, "alpha", "C:/Foxy")),
            Err(EndpointError::RelativeRuntime)
        );
        assert_eq!(
            place_runtime_endpoint(&inputs(
                &abs(&["rt"]),
                Path::new("games"),
                &locks,
                "alpha",
                "C:/Foxy"
            )),
            Err(EndpointError::RelativeSpace)
        );
        assert_eq!(
            place_runtime_endpoint(&inputs(
                &abs(&["rt"]),
                &space,
                Path::new("locks"),
                "alpha",
                "C:/Foxy"
            )),
            Err(EndpointError::RelativeLock)
        );
    }

    #[test]
    fn a_socket_inside_the_game_space_is_rejected() {
        let space = abs(&["games", "alpha"]);
        let runtime = space.join("run");
        let locks = abs(&["locks"]);
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", "C:/Foxy")),
            Err(EndpointError::NestedInSpace)
        );
    }

    #[test]
    fn a_unix_socket_path_over_the_sun_path_limit_is_rejected() {
        let runtime = abs(&[&"a".repeat(100)]);
        let space = abs(&["games", "alpha"]);
        let locks = abs(&["locks"]);
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", "C:/Foxy")),
            Err(EndpointError::PathTooLong)
        );
    }

    #[test]
    fn discovery_metadata_stays_under_server_ipc_and_is_not_the_socket() {
        let runtime = abs(&["rt"]);
        let space = abs(&["games", "alpha"]);
        let locks = abs(&["locks"]);
        let placed =
            place_runtime_endpoint(&inputs(&runtime, &space, &locks, "alpha", "C:/Foxy")).unwrap();
        assert!(
            placed
                .discovery_path
                .starts_with(space.join("server").join("ipc"))
        );
        assert_ne!(placed.discovery_path, placed.socket_path);
        assert_eq!(
            placed
                .discovery_path
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("json")
        );
    }

    #[test]
    fn a_public_tcp_listener_is_rejected() {
        assert_eq!(endpoint_kind("unix"), Ok(EndpointKind::UnixSocket));
        assert_eq!(endpoint_kind("pipe"), Ok(EndpointKind::NamedPipe));
        assert_eq!(endpoint_kind("tcp"), Err(EndpointError::PublicListener));
        assert_eq!(endpoint_kind("udp"), Err(EndpointError::PublicListener));
        assert_eq!(endpoint_kind("http"), Err(EndpointError::UnknownKind));
    }

    #[test]
    fn config_root_normalization_rejects_parent_segments() {
        assert_eq!(
            normalize_config_root(r"C:\Foxy\."),
            Ok("C:/Foxy".to_string())
        );
        assert_eq!(
            normalize_config_root("C:/Foxy/../Other"),
            Err(EndpointError::UnnormalizedRoot)
        );
        assert_eq!(
            normalize_config_root(r"\\server\share"),
            Err(EndpointError::UnnormalizedRoot)
        );
    }

    #[test]
    fn two_spaces_do_not_share_an_endpoint_key() {
        let alpha = endpoint_key("C:/Foxy", "alpha").unwrap();
        let beta = endpoint_key("C:/Foxy", "beta").unwrap();
        let other_root = endpoint_key("D:/Foxy", "alpha").unwrap();
        assert_ne!(alpha, beta);
        assert_ne!(alpha, other_root);
        assert_eq!(alpha.len(), ENDPOINT_KEY_HEX);
    }

    #[test]
    fn the_socket_path_must_not_overlap_the_lock_registry() {
        let runtime = abs(&["rt"]);
        let space = abs(&["games", "alpha"]);
        assert_eq!(
            place_runtime_endpoint(&inputs(&runtime, &space, &runtime, "alpha", "C:/Foxy")),
            Err(EndpointError::OverlapsLock)
        );
    }
}
