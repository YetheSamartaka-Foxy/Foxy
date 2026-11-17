use std::net::Ipv4Addr;

use super::{Arma3RconSettings, RconPassword, ReforgerRconPermission, ReforgerRconSettings};
use crate::core::server::{
    DraftChange, ServerDefinition, ServerInstance, ServerInstanceId, ServerStore, ServerStoreError,
};

/// Typed RCon settings as an operator enters them; the password is set separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RconSettingsInput {
    pub port: u16,
    pub address: Ipv4Addr,
    pub permission: Option<ReforgerRconPermission>,
    pub max_clients: Option<u8>,
}

/// What a draft says about RCon, without any secret.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RconDraftSettings {
    pub port: u16,
    pub address: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<ReforgerRconPermission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_clients: Option<u8>,
    pub password_stored: bool,
}

pub fn draft_settings(instance: &ServerInstance) -> Option<RconDraftSettings> {
    match &instance.definition {
        ServerDefinition::Arma3(settings) => settings.rcon.as_ref().map(|rcon| RconDraftSettings {
            port: rcon.port,
            address: rcon.address,
            permission: None,
            max_clients: None,
            password_stored: rcon.password_ref.is_some(),
        }),
        ServerDefinition::Reforger(settings) => {
            settings.rcon.as_ref().map(|rcon| RconDraftSettings {
                port: rcon.port,
                address: rcon.address,
                permission: Some(rcon.permission),
                max_clients: rcon.max_clients,
                password_stored: rcon.password_ref.is_some(),
            })
        }
    }
}

fn apply_settings(
    definition: &mut ServerDefinition,
    input: Option<RconSettingsInput>,
) -> Result<(), ServerStoreError> {
    match definition {
        ServerDefinition::Arma3(settings) => {
            settings.rcon = match input {
                None => None,
                Some(input) => {
                    if input.permission.is_some() || input.max_clients.is_some() {
                        return Err(ServerStoreError::Invalid(
                            "Arma RCon has no permission or client limit setting".into(),
                        ));
                    }
                    Some(Arma3RconSettings {
                        port: input.port,
                        address: input.address,
                        password_ref: settings
                            .rcon
                            .as_ref()
                            .and_then(|rcon| rcon.password_ref.clone()),
                    })
                }
            };
        }
        ServerDefinition::Reforger(settings) => {
            settings.rcon = match input {
                None => None,
                Some(input) => Some(ReforgerRconSettings {
                    address: input.address,
                    port: input.port,
                    permission: input.permission.unwrap_or(ReforgerRconPermission::Monitor),
                    max_clients: input.max_clients,
                    password_ref: settings
                        .rcon
                        .as_ref()
                        .and_then(|rcon| rcon.password_ref.clone()),
                }),
            };
            if input.is_some() {
                for key in ["address", "port", "permission", "maxClients"] {
                    settings.preserved.rcon.remove(key);
                }
            } else {
                settings.preserved.rcon.clear();
            }
        }
    }
    Ok(())
}

fn set_reference(
    definition: &mut ServerDefinition,
    reference: String,
) -> Result<(), ServerStoreError> {
    let slot = match definition {
        ServerDefinition::Arma3(settings) => {
            settings.rcon.as_mut().map(|rcon| &mut rcon.password_ref)
        }
        ServerDefinition::Reforger(settings) => {
            settings.rcon.as_mut().map(|rcon| &mut rcon.password_ref)
        }
    };
    let slot = slot.ok_or_else(|| {
        ServerStoreError::Invalid("Configure the RCon port before storing its password".into())
    })?;
    *slot = Some(reference);
    Ok(())
}

impl ServerStore {
    pub fn configure_rcon(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        input: Option<RconSettingsInput>,
    ) -> Result<ServerInstance, ServerStoreError> {
        let mut instance = self.show(id)?;
        if instance.revision != expected_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        apply_settings(&mut instance.definition, input)?;
        instance
            .definition
            .validate()
            .map_err(ServerStoreError::Invalid)?;
        let draft = self.plan(DraftChange::Replace {
            instance,
            expected_revision,
        })?;
        let saved = draft.instance.clone();
        self.apply(draft)?;
        Ok(saved)
    }

    /// Stores a new protected value under a fresh reference and points the draft at it.
    /// Earlier references stay readable for committed revisions that still use them.
    pub fn set_rcon_password(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        password: &RconPassword,
    ) -> Result<ServerInstance, ServerStoreError> {
        let mut instance = self.show(id)?;
        if instance.revision != expected_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        let reference = super::new_secret_reference();
        set_reference(&mut instance.definition, reference.clone())?;
        let draft = self.plan(DraftChange::Replace {
            instance,
            expected_revision,
        })?;
        let saved = draft.instance.clone();
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                self.store_config_secret(id, &reference, password.expose())?;
                self.apply_owned(draft)
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)??;
        Ok(saved)
    }

    /// The active committed runtime's RCon endpoint and protected password, for a direct probe.
    pub fn active_rcon_target(
        &self,
        id: &ServerInstanceId,
        control: &crate::core::server::ContentStageControl,
    ) -> Result<(std::net::SocketAddr, RconPassword), ServerStoreError> {
        let plan = self.plan_instance_start(id, control)?;
        let rcon = plan.launch.rcon.ok_or_else(|| {
            ServerStoreError::Invalid(
                "The active runtime of this instance has no RCon settings".into(),
            )
        })?;
        let secret = self.resolve_config_secret(id, &rcon.password_reference)?;
        let password = RconPassword::new(secret).map_err(|_| ServerStoreError::Corrupt)?;
        Ok((rcon.endpoint, password))
    }

    pub fn rcon_password_available(&self, id: &ServerInstanceId) -> Result<bool, ServerStoreError> {
        let instance = self.show(id)?;
        let reference = match &instance.definition {
            ServerDefinition::Arma3(settings) => settings
                .rcon
                .as_ref()
                .and_then(|rcon| rcon.password_ref.clone()),
            ServerDefinition::Reforger(settings) => settings
                .rcon
                .as_ref()
                .and_then(|rcon| rcon.password_ref.clone()),
        };
        match reference {
            Some(reference) => Ok(self.resolve_config_secret(id, &reference).is_ok()),
            None => Ok(false),
        }
    }
}

/// Protected values are bound to one instance id, so a copied definition must not keep the
/// original's reference.
pub(crate) fn clear_password_reference(definition: &mut ServerDefinition) {
    match definition {
        ServerDefinition::Arma3(settings) => {
            if let Some(rcon) = &mut settings.rcon {
                rcon.password_ref = None;
            }
        }
        ServerDefinition::Reforger(settings) => {
            if let Some(rcon) = &mut settings.rcon {
                rcon.password_ref = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arma() -> ServerDefinition {
        ServerDefinition::draft(crate::core::server::ServerDefinitionKind::Arma3, "Server")
    }

    #[test]
    fn settings_keep_the_password_reference_and_disable_clears_it() {
        let mut definition = arma();
        let input = RconSettingsInput {
            port: 2320,
            address: Ipv4Addr::LOCALHOST,
            permission: None,
            max_clients: None,
        };
        assert!(set_reference(&mut definition, format!("secret_{}", "a".repeat(32))).is_err());
        apply_settings(&mut definition, Some(input)).unwrap();
        set_reference(&mut definition, format!("secret_{}", "a".repeat(32))).unwrap();
        apply_settings(
            &mut definition,
            Some(RconSettingsInput {
                port: 2330,
                ..input
            }),
        )
        .unwrap();
        let ServerDefinition::Arma3(settings) = &definition else {
            unreachable!()
        };
        let rcon = settings.rcon.as_ref().unwrap();
        assert_eq!(rcon.port, 2330);
        assert_eq!(
            rcon.password_ref.as_deref(),
            Some(&*format!("secret_{}", "a".repeat(32)))
        );
        definition.validate().unwrap();
        assert!(
            apply_settings(
                &mut definition,
                Some(RconSettingsInput {
                    permission: Some(ReforgerRconPermission::Admin),
                    ..input
                })
            )
            .is_err()
        );
        apply_settings(&mut definition, None).unwrap();
        let ServerDefinition::Arma3(settings) = &definition else {
            unreachable!()
        };
        assert!(settings.rcon.is_none());
    }

    #[test]
    fn arma_rcon_needs_battleye_and_a_port_outside_the_game_bundle() {
        let mut definition = arma();
        let ServerDefinition::Arma3(settings) = &mut definition else {
            unreachable!()
        };
        settings.rcon = Some(Arma3RconSettings {
            port: 2306,
            address: Ipv4Addr::LOCALHOST,
            password_ref: None,
        });
        assert!(definition.validate().is_err());
        let ServerDefinition::Arma3(settings) = &mut definition else {
            unreachable!()
        };
        settings.rcon.as_mut().unwrap().port = 2307;
        definition.validate().unwrap();
        let ServerDefinition::Arma3(settings) = &mut definition else {
            unreachable!()
        };
        settings.battleye = false;
        assert!(definition.validate().is_err());
    }
}
