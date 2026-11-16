use serde::{Deserialize, Serialize};

use super::super::config_import::parser::{CfgValue, Document, Node, NodeKind};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MissionMetadata {
    pub display_name: Option<String>,
    pub author: Option<String>,
    pub declared_addons: Vec<String>,
    pub params: Vec<MissionParameter>,
    pub locations: Vec<MissionMetadataLocation>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionMetadataLocation {
    pub field: String,
    pub source: String,
    pub offset: usize,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionParameter {
    pub name: String,
    pub choices: Vec<String>,
    pub source: String,
    pub offset: usize,
    pub line: usize,
}

pub(super) fn parse_metadata(files: &[(String, Vec<u8>)]) -> MissionMetadata {
    let mut result = MissionMetadata::default();
    for (path, bytes) in files {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let is_description = leaf.eq_ignore_ascii_case("description.ext");
        let is_sqm = leaf.eq_ignore_ascii_case("mission.sqm");
        if !is_description && !is_sqm {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            result
                .warnings
                .push(format!("{path}:byte 0: metadata is not UTF-8; ignored"));
            continue;
        };
        let parsed = match super::super::config_import::parser::parse(text) {
            Ok(document) => document,
            Err(_) => {
                result
                    .warnings
                    .push(format!("{path}: metadata syntax is unsupported; ignored"));
                continue;
            }
        };
        if parsed.unsupported {
            let offset = unsupported_offset(&parsed.nodes)
                .or_else(|| text.find('#'))
                .unwrap_or(0);
            warn_at(
                &mut result,
                path,
                offset,
                text,
                "preprocessor or opaque values were not evaluated",
            );
        }

        let mut flattened = Vec::new();
        flatten(&parsed.nodes, &mut flattened);
        let mut assignments: Vec<_> = flattened
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Assignment { value, .. } => Some((node, value)),
                NodeKind::Class { .. } => None,
            })
            .collect();
        if is_description {
            assignments.retain(|(node, _)| {
                parsed.nodes.iter().any(|root| {
                    root.span.contains(&node.span.start)
                        && matches!(root.kind, NodeKind::Assignment { .. })
                })
            });
        }
        let has_literal_onload_name = is_description
            && assignments.iter().any(|(node, value)| {
                node.name == "onLoadName" && matches!(value, CfgValue::String(_))
            });

        for (node, value) in &assignments {
            match node.name.as_str() {
                "onLoadName" if is_description => {
                    if let CfgValue::String(value) = value {
                        set_display_name(&mut result, value, path, node.span.start, text);
                    } else {
                        warn_at(
                            &mut result,
                            path,
                            node.span.start,
                            text,
                            "display name is not a literal string",
                        );
                    }
                }
                "briefingName" if is_description && !has_literal_onload_name => {
                    if let CfgValue::String(value) = value {
                        set_display_name(&mut result, value, path, node.span.start, text);
                    } else {
                        warn_at(
                            &mut result,
                            path,
                            node.span.start,
                            text,
                            "briefingName is not a literal string",
                        );
                    }
                }
                "briefingName" if is_sqm && result.display_name.is_none() => {
                    if let CfgValue::String(value) = value {
                        set_display_name(&mut result, value, path, node.span.start, text);
                    } else {
                        warn_at(
                            &mut result,
                            path,
                            node.span.start,
                            text,
                            "briefingName is not a literal string",
                        );
                    }
                }
                "author" if (is_sqm || is_description) && result.author.is_none() => {
                    if let CfgValue::String(value) = value {
                        result.author = Some(value.clone());
                        locate(&mut result, "author", path, node.span.start, text);
                    } else {
                        warn_at(
                            &mut result,
                            path,
                            node.span.start,
                            text,
                            "author is not a literal string",
                        );
                    }
                }
                "addons" if is_sqm => match value {
                    CfgValue::Array(values) => {
                        for value in values {
                            if let CfgValue::String(value) = value {
                                if result.declared_addons.len() < 1024 {
                                    result.declared_addons.push(value.clone());
                                }
                            } else {
                                warn_at(
                                    &mut result,
                                    path,
                                    node.span.start,
                                    text,
                                    "addons contains a non-literal value",
                                );
                                break;
                            }
                        }
                        locate(&mut result, "declared_addons", path, node.span.start, text);
                    }
                    _ => warn_at(
                        &mut result,
                        path,
                        node.span.start,
                        text,
                        "addons is not a literal array",
                    ),
                },
                _ => {}
            }
        }
        if is_description {
            extract_params(&mut result, &parsed, path, text);
        }
    }
    if result.display_name.is_none() {
        result
            .warnings
            .push("mission.sqm/description.ext: no literal display name was available".into());
    }
    if result.author.is_none() {
        result
            .warnings
            .push("mission.sqm: no literal author was available".into());
    }
    result.declared_addons.sort();
    result.declared_addons.dedup();
    result
}

fn flatten<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        if let NodeKind::Class { children, .. } = &node.kind {
            flatten(children, out);
        }
    }
}

fn unsupported_offset(nodes: &[Node]) -> Option<usize> {
    for node in nodes {
        match &node.kind {
            NodeKind::Assignment { value, .. } if contains_opaque(value) => {
                return Some(node.span.start);
            }
            NodeKind::Class { children, .. } => {
                if let Some(offset) = unsupported_offset(children) {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn contains_opaque(value: &CfgValue) -> bool {
    match value {
        CfgValue::Opaque(_) => true,
        CfgValue::Array(values) => values.iter().any(contains_opaque),
        _ => false,
    }
}

fn extract_params(result: &mut MissionMetadata, document: &Document, source: &str, text: &str) {
    let Some(params) = document.nodes.iter().find(|node| {
        node.name.eq_ignore_ascii_case("Params") && matches!(node.kind, NodeKind::Class { .. })
    }) else {
        return;
    };
    let NodeKind::Class { children, .. } = &params.kind else {
        return;
    };
    for parameter in children {
        let NodeKind::Class { children, .. } = &parameter.kind else {
            continue;
        };
        let texts = children.iter().find_map(|node| {
            if !node.name.eq_ignore_ascii_case("texts") {
                return None;
            }
            let NodeKind::Assignment {
                array: true, value, ..
            } = &node.kind
            else {
                return None;
            };
            let CfgValue::Array(values) = value else {
                return None;
            };
            values
                .iter()
                .map(|value| match value {
                    CfgValue::String(value) => Some(value.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
        });
        let values = children.iter().find_map(|node| {
            if !node.name.eq_ignore_ascii_case("values") {
                return None;
            }
            let NodeKind::Assignment {
                array: true, value, ..
            } = &node.kind
            else {
                return None;
            };
            let CfgValue::Array(values) = value else {
                return None;
            };
            values
                .iter()
                .map(|value| match value {
                    CfgValue::Number(value) => Some(value.to_string()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
        });
        match (texts, values) {
            (Some(texts), Some(values)) if !texts.is_empty() && texts.len() == values.len() => {
                result.params.push(MissionParameter {
                    name: parameter.name.clone(),
                    choices: values
                        .into_iter()
                        .zip(texts)
                        .map(|(value, text)| format!("{value}: {text}"))
                        .collect(),
                    source: source.to_owned(),
                    offset: parameter.span.start,
                    line: line_at(text, parameter.span.start),
                });
            }
            (None, None) => {}
            _ => warn_at(
                result,
                source,
                parameter.span.start,
                text,
                "Params choices are not matching literal values[] and texts[] arrays",
            ),
        }
    }
}

fn set_display_name(
    result: &mut MissionMetadata,
    value: &str,
    source: &str,
    offset: usize,
    text: &str,
) {
    result.display_name = Some(value.to_owned());
    result
        .locations
        .retain(|location| location.field != "display_name");
    result.locations.push(MissionMetadataLocation {
        field: "display_name".into(),
        source: source.into(),
        offset,
        line: line_at(text, offset),
    });
}

fn locate(result: &mut MissionMetadata, field: &str, source: &str, offset: usize, text: &str) {
    result.locations.push(MissionMetadataLocation {
        field: field.into(),
        source: source.into(),
        offset,
        line: line_at(text, offset),
    });
}

fn warn_at(result: &mut MissionMetadata, source: &str, offset: usize, text: &str, message: &str) {
    result.warnings.push(format!(
        "{source}:{} (byte {offset}): {message}",
        line_at(text, offset)
    ));
}

fn line_at(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}
