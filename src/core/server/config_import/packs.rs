use super::{ConfigurationImportReport, ImportedConfiguration, review};
use crate::core::server::{
    DraftChange, ServerDefinition, ServerInstance, ServerInstanceId, ServerStore, ServerStoreError,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableInstancePack {
    pub format: String,
    pub schema_version: u32,
    pub display_name: String,
    pub definition: ServerDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_import: Option<Box<ImportedConfiguration>>,
    pub installation_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_fields: Vec<String>,
}

impl PortableInstancePack {
    pub fn sanitized(mut self) -> Result<Self, ServerStoreError> {
        self.display_name = "SanitizedServer".into();
        self.omitted_fields
            .push("display_name and server identity replaced with SanitizedServer".into());
        match &mut self.definition {
            ServerDefinition::Arma3(arma) => {
                arma.hostname = "SanitizedServer".into();
                arma.mission_cycle.clear();
                *arma.configuration = Default::default();
                self.omitted_fields.extend([
                    "mission_cycle".into(),
                    "configuration filename overrides".into(),
                ]);
                let mut value =
                    serde_json::to_value(&arma.server).map_err(|_| ServerStoreError::Corrupt)?;
                for field in super::super::Arma3ServerSettings::FIELDS {
                    if matches!(
                        field.kind,
                        super::super::Arma3FieldKind::Text
                            | super::super::Arma3FieldKind::Strings(_)
                            | super::super::Arma3FieldKind::SecretRef
                    ) && value
                        .as_object_mut()
                        .is_some_and(|object| object.remove(field.json_name).is_some())
                    {
                        self.omitted_fields
                            .push(format!("server.{}", field.json_name));
                    }
                }
                arma.server =
                    serde_json::from_value(value).map_err(|_| ServerStoreError::Corrupt)?;
            }
            ServerDefinition::Reforger(reforger) => {
                reforger.name = "SanitizedServer".into();
                reforger.scenario_id = None;
                reforger.addons.clear();
                reforger.cross_platform = None;
                reforger.mods_required_by_default = None;
                reforger.game_properties = Default::default();
                reforger.preserved = Default::default();
                reforger.redacted_secret_paths.clear();
                self.omitted_fields.extend([
                    "scenario_id".into(),
                    "addons".into(),
                    "preserved server.json fields".into(),
                ]);
            }
        }
        if self.configuration_import.take().is_some() {
            self.omitted_fields
                .push("operator templates, source references and path hints".into());
        }
        self.validate()?;
        Ok(self)
    }
    pub fn read(path: &Path) -> Result<Self, ServerStoreError> {
        super::super::store::check_regular_file(path)?;
        let bytes = super::super::store::read_bounded(path).map_err(|_| ServerStoreError::Io)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| ServerStoreError::Invalid("Invalid portable instance pack".into()))
    }
    pub fn write_new(&self, path: &Path) -> Result<(), ServerStoreError> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        super::super::store::check_directory_chain(parent)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(ServerStoreError::Invalid(
                "Portable instance pack exceeds 4 MiB".into(),
            ));
        }
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
        file.write_all(&bytes).map_err(|_| ServerStoreError::Io)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        file.persist_noclobber(path).map_err(|_| {
            ServerStoreError::Invalid(
                "Pack export destination already exists or cannot be created".into(),
            )
        })?;
        Ok(())
    }
    fn validate(&self) -> Result<(), ServerStoreError> {
        if self.format != "foxy-server-instance" || self.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        self.definition
            .validate()
            .map_err(ServerStoreError::Invalid)?;
        if let Some(imported) = &self.configuration_import {
            imported.validate().map_err(ServerStoreError::Invalid)?;
        }
        Ok(())
    }
}

pub struct InstancePackImportPlan {
    pub report: ConfigurationImportReport,
    plan: super::super::store::DraftPlan,
    context: super::super::ServerContext,
}

impl InstancePackImportPlan {
    pub fn set_paths_reviewed(&mut self, reviewed: bool) {
        self.plan.set_import_paths_reviewed(reviewed);
        if let Some(imported) = &mut self.report.instance.configuration_import {
            imported.paths_reviewed = reviewed;
        }
        self.report.fingerprint = review::fingerprint(&self.report.instance);
    }
}

impl ServerStore {
    pub fn export_instance_pack(
        &self,
        id: &ServerInstanceId,
    ) -> Result<PortableInstancePack, ServerStoreError> {
        let mut instance = self.show(id)?;
        if let Some(bindings) = self.content_bindings(id)?.document {
            let imported = instance
                .configuration_import
                .get_or_insert_with(Box::default);
            imported.source_references = bindings.bindings;
            imported.unresolved_paths =
                review::derived_paths(imported).map_err(ServerStoreError::Invalid)?;
            imported.paths_reviewed = imported.unresolved_paths.is_empty();
        }
        let omitted_fields = instance
            .configuration_import
            .as_ref()
            .map(|imported| imported.pack_omissions.clone())
            .unwrap_or_default();
        Ok(PortableInstancePack {
            format: "foxy-server-instance".into(),
            schema_version: 1,
            display_name: instance.display_name,
            definition: instance.definition,
            configuration_import: instance.configuration_import,
            installation_required: instance.installation_id.is_some(),
            omitted_fields,
        })
    }
    pub fn plan_instance_pack_import(
        &self,
        mut pack: PortableInstancePack,
        name: &str,
    ) -> Result<InstancePackImportPlan, ServerStoreError> {
        if !pack.omitted_fields.is_empty() {
            pack.configuration_import
                .get_or_insert_with(Box::default)
                .pack_omissions = pack.omitted_fields.clone();
        }
        if let Some(imported) = &mut pack.configuration_import {
            imported.unresolved_paths =
                review::derived_paths(imported).map_err(ServerStoreError::Invalid)?;
            imported.paths_reviewed = imported.unresolved_paths.is_empty();
        }
        pack.validate()?;
        let mut instance = ServerInstance::draft(
            self.context().space_id(),
            pack.definition.kind(),
            if name.trim().is_empty() {
                &pack.display_name
            } else {
                name
            },
        )
        .map_err(ServerStoreError::Invalid)?;
        instance.definition = pack.definition;
        instance.configuration_import = pack.configuration_import;
        if let Some(imported) = &mut instance.configuration_import {
            imported.paths_reviewed = imported.unresolved_paths.is_empty();
        }
        let plan = self.plan(DraftChange::Import { instance })?;
        let instance = plan.instance.clone();
        let references =
            review::all_secret_references(&instance).map_err(ServerStoreError::Invalid)?;
        let unresolved_paths = instance
            .configuration_import
            .as_ref()
            .map(|i| i.unresolved_paths.clone())
            .unwrap_or_default();
        let mut warnings = vec!["Imported packs create a new unbound instance. Adopt/bind a local installation and review paths and credentials before activation.".into()];
        if !super::super::missions::cycle_library_sources(&instance.definition).is_empty() {
            warnings.push("Mission library references are preserved as unresolved intent; mission bytes are not included. Import or acquire the missions locally and explicitly rebind the cycle before staging.".into());
        }
        if !pack.omitted_fields.is_empty() {
            warnings.push(format!(
                "This is a sanitized, incomplete pack. Omitted or replaced fields: {}",
                pack.omitted_fields.join(", ")
            ));
        }
        if instance
            .configuration_import
            .as_ref()
            .is_some_and(|imported| !imported.source_references.is_empty())
        {
            warnings.push("Content source identities, addon roles and order are preserved as unresolved intent. Register local source mappings and review content bindings/staging separately; no source path was opened.".into());
        }
        if let ServerDefinition::Arma3(arma) = &instance.definition
            && self.load()?.instances.iter().any(|existing| matches!(&existing.definition, ServerDefinition::Arma3(other) if other.game_port.abs_diff(arma.game_port) <= 4))
        {
            warnings.push("The imported Arma port bundle overlaps an existing draft. Choose unused ports before activation.".into());
        }
        let report = ConfigurationImportReport { instance_id: instance.id.clone(), expected_revision: 0, fingerprint: review::fingerprint(&instance), instance, warnings, unresolved_paths, secret_references: references, diff: vec!["New instance identity; installation, runtime, content bytes, credentials and saves are not copied.".into()], writes_performed: false };
        Ok(InstancePackImportPlan {
            report,
            plan,
            context: self.context().clone(),
        })
    }
    pub fn apply_instance_pack_import(
        &self,
        plan: InstancePackImportPlan,
    ) -> Result<ServerInstance, ServerStoreError> {
        if plan.context != *self.context()
            || plan.report.fingerprint != review::fingerprint(&plan.report.instance)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let retained = plan.plan;
        if retained.instance.configuration_import != plan.report.instance.configuration_import {
            return Err(ServerStoreError::Invalid(
                "Re-review the pack after changing its path decision".into(),
            ));
        }
        let instance = retained.instance.clone();
        self.apply(retained)?;
        Ok(instance)
    }
}
