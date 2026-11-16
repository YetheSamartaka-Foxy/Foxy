use super::{OwnerLifetime, OwnerSnapshot, attach};
use crate::core::game::spaces::{GAMES_REGISTRY_FILE, GamesRegistryFile};
use crate::core::server::ServerContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateBlockerKind {
    Sessions(u32),
    Detached,
    /// An owner record exists but the owner could not be inspected, so sessions are unknown.
    Unreachable,
    /// The game space list could not be read, so owners could not be checked at all.
    RegistryUnreadable,
}

/// A reason replacing the binary now could end supervised servers or orphan a detached owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateBlocker {
    pub space_name: String,
    pub kind: UpdateBlockerKind,
}

pub(crate) fn blocks_update(snapshot: &OwnerSnapshot) -> Option<UpdateBlockerKind> {
    if snapshot.lifetime == OwnerLifetime::Detached {
        Some(UpdateBlockerKind::Detached)
    } else if snapshot.active_sessions > 0 {
        Some(UpdateBlockerKind::Sessions(snapshot.active_sessions))
    } else {
        None
    }
}

fn owner_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Inspects the owner of every server-capable game space under `root`. Only a missing or dead
/// owner counts as no owner; an owner that cannot be inspected blocks.
pub(crate) fn update_blockers(root: &std::path::Path) -> Vec<UpdateBlocker> {
    let registry = match std::fs::read(root.join(GAMES_REGISTRY_FILE)) {
        Ok(bytes) => serde_json::from_slice::<GamesRegistryFile>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => None,
    };
    let Some(registry) = registry else {
        return vec![UpdateBlocker {
            space_name: String::new(),
            kind: UpdateBlockerKind::RegistryUnreadable,
        }];
    };
    registry
        .game_spaces
        .iter()
        .filter(|space| {
            crate::core::game::registry()
                .get(&space.game_id)
                .is_some_and(|game| game.server_capabilities().lifecycle)
        })
        .filter_map(|space| {
            let context = ServerContext::capture(root, Some(&space.id)).ok()?;
            let kind = match attach(&context) {
                Ok(None) => None,
                Err(error) if owner_absent(&error) => None,
                Err(_) => Some(UpdateBlockerKind::Unreachable),
                Ok(Some(attachment)) => match attachment.inspect() {
                    Ok(snapshot) => blocks_update(&snapshot),
                    Err(error) if owner_absent(&error) => None,
                    Err(_) => Some(UpdateBlockerKind::Unreachable),
                },
            }?;
            Some(UpdateBlocker {
                space_name: space.display_name.clone(),
                kind,
            })
        })
        .collect()
}
