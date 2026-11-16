use super::{
    ConfigurationImportInput, ConfigurationImportReport, ConfigurationOwnership, ConfigurationRole,
    ImportedConfiguration, ImportedConfigurationFile, mapping,
    parser::{self, CfgValue, Node, NodeKind},
};
use crate::core::server::{
    DraftChange, ServerDefinitionKind, ServerInstance, ServerInstanceId, ServerStore,
    ServerStoreError,
};
use std::path::PathBuf;

pub struct ConfigurationImportPlan {
    pub report: ConfigurationImportReport,
    context: super::super::ServerContext,
    sources: Vec<(PathBuf, blake3::Hash)>,
    secrets: Vec<(String, String)>,
}

impl ConfigurationImportPlan {
    pub fn set_paths_reviewed(&mut self, reviewed: bool) {
        if let Some(imported) = &mut self.report.instance.configuration_import {
            imported.paths_reviewed = reviewed;
        }
        self.report.fingerprint = fingerprint(&self.report.instance);
    }
}

pub(super) fn fingerprint(instance: &ServerInstance) -> String {
    blake3::hash(&serde_json::to_vec(instance).unwrap_or_default())
        .to_hex()
        .to_string()
}

fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("password") || lower.contains("secret") || lower.contains("token")
}

fn assignments<'a>(nodes: &'a [Node], found: &mut Vec<&'a Node>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Class { children, .. } => assignments(children, found),
            NodeKind::Assignment { .. } => found.push(node),
        }
    }
}

fn references(
    source: &str,
    allow_plain: bool,
) -> Result<Vec<(std::ops::Range<usize>, String)>, String> {
    let document = parser::parse(source)?;
    if document.unsupported {
        return Err("Includes, macros or expressions cannot be safely snapshotted. Original operator source was left unchanged; resolve them before importing.".into());
    }
    let mut nodes = Vec::new();
    assignments(&document.nodes, &mut nodes);
    let mut result = Vec::new();
    for node in nodes.into_iter().filter(|node| secret_name(&node.name)) {
        let NodeKind::Assignment {
            value: CfgValue::String(value),
            value_span,
            append: false,
            ..
        } = &node.kind
        else {
            return Err("Secret directives must have a single quoted value before import".into());
        };
        if !allow_plain && !valid_reference_value(value) {
            return Err("Imported configuration contains an unprotected secret directive".into());
        }
        result.push((value_span.clone(), value.clone()));
    }
    Ok(result)
}

fn valid_reference_value(value: &str) -> bool {
    value
        .strip_prefix("foxy-secret:secret_")
        .is_some_and(|tail| {
            tail.len() == 32
                && tail
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

pub(super) fn validate_imported(imported: &ImportedConfiguration) -> Result<(), String> {
    if imported.files.len() > 3
        || imported.unresolved_paths.len() > 1024
        || imported.source_references.len() > 256
    {
        return Err("Imported configuration exceeds its limits".into());
    }
    super::super::addons::order_bindings(
        imported
            .source_references
            .iter()
            .map(|source| source.binding.clone())
            .collect(),
    )
    .map_err(|_| "Invalid portable content reference order or identity")?;
    for source in &imported.source_references {
        if source.binding.source_id != source.profile.name
            || source.profile.folder_id != source.folder.id
        {
            return Err(
                "Portable content references must preserve exact profile and folder identities"
                    .into(),
            );
        }
        if !source.profile.remote_url.is_empty() {
            let url = reqwest::Url::parse(&source.profile.remote_url)
                .map_err(|_| "Invalid portable source URL")?;
            if !matches!(url.scheme(), "https" | "http")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(
                    "Portable source URL must not contain credentials, query or fragment".into(),
                );
            }
        }
        if source.profile.addons.len() > 4096
            || source.profile.addons.iter().any(|addon| {
                addon.is_empty()
                    || addon.len() > 256
                    || addon.contains(['/', '\\', ':'])
                    || addon == "."
                    || addon == ".."
            })
        {
            return Err("Invalid portable addon reference".into());
        }
    }
    let mut seen = Vec::new();
    for file in &imported.files {
        if file.ownership != ConfigurationOwnership::Operator {
            return Err("Stored raw templates require explicit operator ownership".into());
        }
        if seen.contains(&file.role) {
            return Err("Duplicate imported configuration role".into());
        }
        seen.push(file.role);
        references(&file.template, false)?;
    }
    if derived_paths(imported)?
        .iter()
        .any(|path| !imported.unresolved_paths.contains(path))
    {
        return Err("Imported path review does not cover the operator template".into());
    }
    Ok(())
}

pub(super) fn secret_references(imported: &ImportedConfiguration) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for file in &imported.files {
        for (_, value) in references(&file.template, false)? {
            let reference = value
                .strip_prefix("foxy-secret:")
                .ok_or("Invalid protected reference")?
                .to_string();
            if !found.contains(&reference) {
                found.push(reference);
            }
        }
    }
    Ok(found)
}

fn path_values(nodes: &[Node], paths: &mut Vec<String>) -> Result<(), String> {
    fn value(v: &CfgValue, paths: &mut Vec<String>) -> Result<(), String> {
        match v {
            CfgValue::String(text) => {
                let bytes = text.as_bytes();
                let absolute = text.starts_with('/')
                    || text.starts_with("\\\\")
                    || text.contains("..")
                    || (bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'/' | b'\\'));
                if absolute && !paths.contains(text) {
                    if paths.len() == 1024 {
                        return Err(
                            "Configuration has too many unresolved paths for a complete review"
                                .into(),
                        );
                    }
                    paths.push(text.clone());
                }
            }
            CfgValue::Array(items) => {
                for item in items {
                    value(item, paths)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    for node in nodes {
        match &node.kind {
            NodeKind::Class { children, .. } => path_values(children, paths)?,
            NodeKind::Assignment { value: v, .. } => value(v, paths)?,
        }
    }
    Ok(())
}

pub(super) fn derived_paths(imported: &ImportedConfiguration) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for file in &imported.files {
        path_values(&parser::parse(&file.template)?.nodes, &mut paths)?;
    }
    for source in &imported.source_references {
        for path in [&source.profile.local_path, &source.folder.path] {
            let text = path.to_str().ok_or("Portable content path is not UTF-8")?;
            if !text.is_empty() && !paths.iter().any(|path| path == text) {
                if paths.len() == 1024 {
                    return Err(
                        "Configuration has too many unresolved paths for a complete review".into(),
                    );
                }
                paths.push(text.to_owned());
            }
        }
    }
    Ok(paths)
}

pub(in crate::core::server) fn render_imported(
    instance: &ServerInstance,
    files: &mut Vec<super::super::configuration::ConfigurationFile>,
    warnings: &mut Vec<String>,
) -> Result<(), ServerStoreError> {
    let Some(imported) = &instance.configuration_import else {
        return Ok(());
    };
    let super::super::ServerDefinition::Arma3(arma) = &instance.definition else {
        return Err(ServerStoreError::UnsupportedGame);
    };
    imported.validate().map_err(ServerStoreError::Invalid)?;
    for imported_file in &imported.files {
        let name = match imported_file.role {
            ConfigurationRole::Server => arma.configuration.server_file().to_string(),
            ConfigurationRole::Basic => arma.configuration.basic_file().to_string(),
            ConfigurationRole::Profile => {
                format!("{}.Arma3Profile", arma.configuration.profile_name())
            }
        };
        let file = super::super::configuration::ConfigurationFile {
            relative_path: name.clone(),
            contents: imported_file.template.clone(),
            blake3: blake3::hash(imported_file.template.as_bytes())
                .to_hex()
                .to_string(),
        };
        if let Some(existing) = files.iter_mut().find(|f| f.relative_path == name) {
            *existing = file;
        } else {
            files.push(file);
        }
        if imported_file.role == ConfigurationRole::Profile {
            let intent = files
                .iter_mut()
                .find(|f| f.relative_path == "launch-intent.json")
                .ok_or(ServerStoreError::Corrupt)?;
            let mut value: serde_json::Value =
                serde_json::from_str(&intent.contents).map_err(|_| ServerStoreError::Corrupt)?;
            value["profile_name"] = arma.configuration.profile_name().into();
            intent.contents =
                serde_json::to_string_pretty(&value).map_err(|_| ServerStoreError::Corrupt)?;
            intent.blake3 = blake3::hash(intent.contents.as_bytes())
                .to_hex()
                .to_string();
        }
    }
    if !imported.files.is_empty() {
        warnings.push("Operator-owned configuration is preserved verbatim as a redacted template; managed controls do not rewrite it.".into());
    }
    if !imported.unresolved_paths.is_empty() && !imported.paths_reviewed {
        warnings.push(
            "Imported paths are unresolved and require explicit review before activation.".into(),
        );
    }
    Ok(())
}

pub(super) fn all_secret_references(instance: &ServerInstance) -> Result<Vec<String>, String> {
    let mut refs = match &instance.configuration_import {
        Some(imported) => secret_references(imported)?,
        None => vec![],
    };
    if let super::super::ServerDefinition::Arma3(arma) = &instance.definition {
        for reference in [
            &arma.server.password_ref,
            &arma.server.password_admin_ref,
            &arma.server.server_command_password_ref,
        ]
        .into_iter()
        .flatten()
        {
            if !refs.contains(reference) {
                refs.push(reference.clone());
            }
        }
    }
    Ok(refs)
}

impl ServerStore {
    pub(in crate::core::server) fn validate_configuration_bindings(
        &self,
        instance: &ServerInstance,
    ) -> Result<(), ServerStoreError> {
        if let Some(imported) = &instance.configuration_import {
            if !imported.unresolved_paths.is_empty() && !imported.paths_reviewed {
                return Err(ServerStoreError::Invalid(
                    "Review unresolved operator paths before activation".into(),
                ));
            }
            if imported.files.iter().any(|f| {
                f.role == ConfigurationRole::Profile
                    && references(&f.template, false).is_ok_and(|refs| !refs.is_empty())
            }) {
                return Err(ServerStoreError::Invalid("Protected directives in imported profiles require an operator review; no profile was launched".into()));
            }
        }
        for reference in all_secret_references(instance).map_err(ServerStoreError::Invalid)? {
            self.resolve_config_secret(&instance.id, &reference)
                .map_err(|_| {
                    ServerStoreError::Invalid(
                        "A protected credential reference is unresolved for this instance".into(),
                    )
                })?;
        }
        Ok(())
    }
}

impl ServerStore {
    pub fn plan_configuration_import(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        inputs: &[ConfigurationImportInput],
    ) -> Result<ConfigurationImportPlan, ServerStoreError> {
        let mut instance = self.show(id)?;
        let before = super::super::configuration::render(&instance)?;
        if instance.revision != expected_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        if instance.definition.kind() != ServerDefinitionKind::Arma3 {
            return Err(ServerStoreError::UnsupportedGame);
        }
        if inputs.is_empty() || inputs.len() > 3 {
            return Err(ServerStoreError::Invalid(
                "Select one to three distinct configuration files".into(),
            ));
        }
        let mut imported = instance
            .configuration_import
            .as_deref()
            .cloned()
            .unwrap_or_default();
        let mut sources = Vec::new();
        let mut secrets = Vec::new();
        let mut warnings = Vec::new();
        let mut diff = Vec::new();
        let mut roles = Vec::new();
        for input in inputs {
            if roles.contains(&input.role) {
                return Err(ServerStoreError::Invalid(
                    "Duplicate configuration file role".into(),
                ));
            }
            roles.push(input.role);
            super::super::store::check_regular_file(&input.path)?;
            let bytes =
                super::super::store::read_bounded(&input.path).map_err(|_| ServerStoreError::Io)?;
            let source = std::str::from_utf8(&bytes).map_err(|_| {
                ServerStoreError::Invalid("Configuration must be UTF-8 text".into())
            })?;
            let sensitive = references(source, true).map_err(ServerStoreError::Invalid)?;
            let without_values = parser::replace_spans(
                source,
                &sensitive
                    .iter()
                    .map(|(span, _)| (span.clone(), String::new()))
                    .collect::<Vec<_>>(),
            )
            .map_err(ServerStoreError::Invalid)?;
            let mut replacements = Vec::new();
            for (span, value) in sensitive {
                if valid_reference_value(&value) {
                    continue;
                }
                if value == "<redacted secret reference>" {
                    return Err(ServerStoreError::Invalid("Masked passwords cannot replace credential values. Import a portable pack or bind an actual credential source.".into()));
                }
                if !value.is_empty()
                    && ((value.len() >= 4 && without_values.contains(&value))
                        || without_values.contains(&super::super::arma3_config::quoted(&value)))
                {
                    return Err(ServerStoreError::Invalid("Secret text also occurs outside its directive; remove the extra occurrence before importing".into()));
                }
                let reference = format!("secret_{:032x}", rand::random::<u128>());
                replacements.push((
                    span,
                    super::super::arma3_config::quoted(&format!("foxy-secret:{reference}")),
                ));
                secrets.push((reference, value));
            }
            let template =
                parser::replace_spans(source, &replacements).map_err(ServerStoreError::Invalid)?;
            let document = parser::parse(&template).map_err(ServerStoreError::Invalid)?;
            let mut candidate = instance.clone();
            if let super::super::ServerDefinition::Arma3(arma) = &mut candidate.definition {
                match input.role {
                    ConfigurationRole::Server => {
                        let super::super::ServerDefinition::Arma3(defaults) =
                            super::super::ServerDefinition::draft(
                                ServerDefinitionKind::Arma3,
                                &candidate.display_name,
                            )
                        else {
                            unreachable!()
                        };
                        arma.hostname = defaults.hostname;
                        arma.max_players = defaults.max_players;
                        arma.persistent = defaults.persistent;
                        arma.battleye = defaults.battleye;
                        arma.verify_signatures = defaults.verify_signatures;
                        *arma.server = Default::default();
                        arma.mission_cycle.clear();
                    }
                    ConfigurationRole::Basic => *arma.basic = Default::default(),
                    ConfigurationRole::Profile => arma.difficulty = None,
                }
            }
            fn needs_operator(nodes: &[Node]) -> bool {
                nodes.iter().any(|node| match &node.kind {
                    NodeKind::Assignment { append, .. } => *append,
                    NodeKind::Class { base, children } => {
                        base.is_some() || needs_operator(children)
                    }
                })
            }
            if needs_operator(&document.nodes) {
                if input.ownership == ConfigurationOwnership::Managed {
                    return Err(ServerStoreError::Invalid("Append assignments and inherited classes require operator ownership; managed conversion would lose their semantics".into()));
                }
                warnings.push("Append assignments or inherited classes are preserved in the operator file and may not be fully represented by managed diagnostics.".into());
            }
            match mapping::apply(&mut candidate, input.role, &document) {
                Ok(unknown) => {
                    warnings.extend(unknown);
                    instance = candidate;
                }
                Err(_) if input.ownership == ConfigurationOwnership::Operator => {
                    instance = candidate;
                    warnings.push("Some operator values cannot be represented in managed controls; original template remains authoritative.".into());
                }
                Err(error) => return Err(ServerStoreError::Invalid(error)),
            }
            path_values(&document.nodes, &mut imported.unresolved_paths)
                .map_err(ServerStoreError::Invalid)?;
            imported.files.retain(|file| file.role != input.role);
            if input.ownership == ConfigurationOwnership::Operator {
                imported.files.push(ImportedConfigurationFile {
                    role: input.role,
                    ownership: input.ownership,
                    template,
                });
            }
            diff.push(format!(
                "{:?}: import {:?} ownership; previous file selection replaced only after apply",
                input.role, input.ownership
            ));
            sources.push((input.path.clone(), blake3::hash(&bytes)));
        }
        imported.paths_reviewed = imported.unresolved_paths.is_empty();
        instance.configuration_import = Some(Box::new(imported));
        instance
            .validate(self.context().space_id(), instance.definition.kind())
            .map_err(ServerStoreError::Invalid)?;
        let unresolved_paths = instance
            .configuration_import
            .as_ref()
            .unwrap()
            .unresolved_paths
            .clone();
        let after = super::super::configuration::render(&instance)?;
        for file in &after.files {
            let old = before
                .files
                .iter()
                .find(|old| old.relative_path == file.relative_path)
                .map(|f| f.contents.as_str())
                .unwrap_or_default();
            if old != file.contents {
                diff.push(format!(
                    "--- {} (previous)\n{}\n+++ {} (reviewed)\n{}",
                    file.relative_path,
                    old.lines()
                        .map(|line| format!("- {line}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    file.relative_path,
                    file.contents
                        .lines()
                        .map(|line| format!("+ {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
        let secret_references =
            all_secret_references(&instance).map_err(ServerStoreError::Invalid)?;
        let report = ConfigurationImportReport {
            instance_id: id.clone(),
            expected_revision,
            fingerprint: fingerprint(&instance),
            instance,
            warnings,
            unresolved_paths,
            secret_references,
            diff,
            writes_performed: false,
        };
        Ok(ConfigurationImportPlan {
            report,
            context: self.context().clone(),
            sources,
            secrets,
        })
    }

    pub fn apply_configuration_import(
        &self,
        plan: ConfigurationImportPlan,
    ) -> Result<ServerInstance, ServerStoreError> {
        if plan.context != *self.context()
            || plan.report.fingerprint != fingerprint(&plan.report.instance)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let draft = self.plan(DraftChange::Replace {
            instance: plan.report.instance,
            expected_revision: plan.report.expected_revision,
        })?;
        for (path, expected) in &plan.sources {
            super::super::store::check_regular_file(path)?;
            if blake3::hash(
                &super::super::store::read_bounded(path).map_err(|_| ServerStoreError::Io)?,
            ) != *expected
            {
                return Err(ServerStoreError::StaleRevision);
            }
        }
        let instance = draft.instance.clone();
        super::super::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                for (reference, value) in &plan.secrets {
                    self.store_config_secret(&instance.id, reference, value)?;
                }
                self.apply_owned(draft)
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)??;
        Ok(instance)
    }
}
