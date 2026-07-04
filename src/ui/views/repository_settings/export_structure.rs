use super::normalize_local_path_for_compare;
use crate::core::db::{FoxyDb, params};
use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;
use log::{info, warn};
use rfd::FileDialog;
use std::collections::BTreeMap;
use tokio::runtime::Runtime;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryStructureFile {
    path: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryStructureAddon {
    name: String,
    files: Vec<RepositoryStructureFile>,
}

impl Foxy {
    pub(super) fn export_repository_structure_to_file(&mut self, repo_index: usize) {
        let Some(repo) = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .cloned()
        else {
            self.show_error_toast(self.t("Failed to export repository structure."));
            return;
        };

        let safe_name = safe_export_file_stem(&repo.name);
        let Some(path) = crate::ui::app::agent_support::save_file(|| {
            FileDialog::new()
                .set_file_name(format!("{safe_name}_repository_structure.txt"))
                .add_filter("Text files", &["txt"])
                .add_filter("All files", &["*"])
                .save_file()
        }) else {
            return;
        };

        let structure = match load_repository_structure(&repo.address, &repo.path) {
            Ok(addons) => addons,
            Err(err) => {
                warn!("Failed to load repository structure: {}", err);
                self.show_error_toast(self.t("Failed to export repository structure."));
                return;
            }
        };
        let export_text = build_repository_structure_text(&repo.name, &structure);

        match std::fs::write(&path, export_text) {
            Ok(()) => {
                info!(
                    "Repository structure exported to file: {}",
                    crate::core::utils::format::sanitize_log_path(&path)
                );
                self.show_success_toast(self.t("Repository structure exported to file."));
            }
            Err(err) => {
                warn!("Failed to write repository structure export: {}", err);
                self.show_error_toast(self.t("Failed to export repository structure."));
            }
        }
    }
}

fn load_repository_structure(
    repo_address: &str,
    repo_local_path: &str,
) -> Result<Vec<RepositoryStructureAddon>, String> {
    let rt = Runtime::new().map_err(|err| err.to_string())?;
    let repo_url = Foxy::normalize_repo_url(repo_address);

    let rows: Result<_, String> = rt.block_on(async {
        let db = FoxyDb::from_handle(crate::core::tasks::init_database::init_database().await);
        let repository_rows = db
            .query_all(
                "SELECT id, local_path FROM repositories WHERE remote_url = ?",
                params![repo_url],
            )
            .await
            .map_err(|err| err.to_string())?;
        let repository_candidates = repository_rows
            .into_iter()
            .filter_map(|row| Some((row.get_i64("id").ok()?, row.get_string("local_path").ok()?)))
            .collect::<Vec<_>>();
        let repository_id =
            select_repository_id_for_export(&repository_candidates, repo_local_path).ok_or_else(
                || "Repository database row was not found for selected instance".to_string(),
            )?;

        db.query_all(
            r#"
            SELECT a.name AS addon_name,
                   a.remote_path AS addon_remote_path,
                   f.name AS file_name,
                   f.remote_path AS file_remote_path,
                   COALESCE(f.length, 0) AS size_bytes
              FROM repositories r
              JOIN repository_addons ra ON ra.repository_id = r.id
              JOIN addons a ON a.id = ra.addon_id
              LEFT JOIN addon_files af ON af.addon_id = a.id
              LEFT JOIN files f ON f.id = af.file_id
             WHERE r.id = ?
             ORDER BY lower(a.name), a.name, f.data_order, lower(f.name), f.name
            "#,
            params![repository_id],
        )
        .await
        .map_err(|err| err.to_string())
    });

    let mut by_addon: BTreeMap<String, Vec<RepositoryStructureFile>> = BTreeMap::new();
    for row in rows? {
        let addon_name = row
            .get_string("addon_name")
            .map_err(|err| err.to_string())?;
        let addon_remote_path = row.get_string("addon_remote_path").unwrap_or_default();
        let file_name = row.get_string("file_name").unwrap_or_default();
        if file_name.trim().is_empty() {
            by_addon.entry(addon_name).or_default();
            continue;
        }
        let file_remote_path = row.get_string("file_remote_path").unwrap_or_default();
        let size_bytes = row.get_i64("size_bytes").unwrap_or(0).max(0) as u64;
        by_addon
            .entry(addon_name)
            .or_default()
            .push(RepositoryStructureFile {
                path: repository_file_display_path(
                    &addon_remote_path,
                    &file_remote_path,
                    &file_name,
                ),
                size_bytes,
            });
    }

    Ok(by_addon
        .into_iter()
        .map(|(name, files)| RepositoryStructureAddon { name, files })
        .collect())
}

fn select_repository_id_for_export(
    candidates: &[(i64, String)],
    repo_local_path: &str,
) -> Option<i64> {
    if let Some((id, _)) = candidates
        .iter()
        .find(|(_, local_path)| local_path == repo_local_path)
    {
        return Some(*id);
    }

    let selected_path = normalize_local_path_for_compare(repo_local_path);
    if !selected_path.is_empty()
        && let Some((id, _)) = candidates
            .iter()
            .find(|(_, local_path)| normalize_local_path_for_compare(local_path) == selected_path)
    {
        return Some(*id);
    }

    (candidates.len() == 1).then_some(candidates[0].0)
}

fn build_repository_structure_text(
    repository_name: &str,
    addons: &[RepositoryStructureAddon],
) -> String {
    let total_size_bytes = addons
        .iter()
        .flat_map(|addon| &addon.files)
        .map(|file| file.size_bytes)
        .sum::<u64>();
    let mut lines = Vec::new();
    lines.push(format!(
        "{repository_name} ({})",
        fmt_bytes(total_size_bytes)
    ));

    for addon in addons {
        let addon_size_bytes = addon.files.iter().map(|file| file.size_bytes).sum::<u64>();
        lines.push(format!("-{} ({})", addon.name, fmt_bytes(addon_size_bytes)));
        for file in &addon.files {
            lines.push(format!("-- {} ({})", file.path, fmt_bytes(file.size_bytes)));
        }
    }

    lines.join("\n")
}

fn repository_file_display_path(
    addon_remote_path: &str,
    file_remote_path: &str,
    file_name: &str,
) -> String {
    let addon_path = normalize_export_path(addon_remote_path);
    let file_path = normalize_export_path(file_remote_path);

    if !addon_path.is_empty() && file_path.len() > addon_path.len() {
        let prefix = format!("{addon_path}/");
        if let Some(relative) = file_path.strip_prefix(&prefix)
            && !relative.trim().is_empty()
        {
            return relative.to_string();
        }
    }

    let normalized_name = normalize_export_path(file_name);
    normalized_name
        .rsplit('/')
        .next()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(file_name)
        .to_string()
}

fn normalize_export_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn safe_export_file_stem(name: &str) -> String {
    let mut safe = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    while safe.contains("__") {
        safe = safe.replace("__", "_");
    }
    safe = safe.trim_matches('_').to_string();
    if safe.is_empty() {
        "repository".to_string()
    } else {
        safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_structure_text_groups_files_with_sizes() {
        let addons = vec![
            RepositoryStructureAddon {
                name: "@ace3".to_string(),
                files: vec![
                    RepositoryStructureFile {
                        path: "addons/ace_advanced_ballistics.pbo".to_string(),
                        size_bytes: 1_048_576,
                    },
                    RepositoryStructureFile {
                        path: "addons/ace_arsenal.pbo".to_string(),
                        size_bytes: 512,
                    },
                ],
            },
            RepositoryStructureAddon {
                name: "@bwa3".to_string(),
                files: vec![RepositoryStructureFile {
                    path: "addons/bwa3_attachments.pbo".to_string(),
                    size_bytes: 2_097_152,
                }],
            },
        ];

        assert_eq!(
            build_repository_structure_text("MAIN", &addons),
            "MAIN (3.0 MB)\n-@ace3 (1.0 MB)\n-- addons/ace_advanced_ballistics.pbo (1.0 MB)\n-- addons/ace_arsenal.pbo (512 B)\n-@bwa3 (2.0 MB)\n-- addons/bwa3_attachments.pbo (2.0 MB)"
        );
    }

    #[test]
    fn repository_file_display_path_prefers_path_relative_to_addon() {
        assert_eq!(
            repository_file_display_path(
                "https://example.test/repo/@ace3",
                "https://example.test/repo/@ace3/addons/ace_main.pbo",
                "ace_main.pbo",
            ),
            "addons/ace_main.pbo"
        );
        assert_eq!(
            repository_file_display_path(
                "",
                "https://example.test/repo/@ace3/addons/ace_main.pbo",
                "ace_main.pbo"
            ),
            "ace_main.pbo"
        );
    }

    #[test]
    fn repository_id_selection_accepts_normalized_path_match() {
        let candidates = vec![
            (1, "D:\\Repos\\Other".to_string()),
            (2, "D:\\Repos\\TFR Main".to_string()),
        ];

        assert_eq!(
            select_repository_id_for_export(&candidates, "D:/Repos/TFR Main/"),
            Some(2)
        );
    }

    #[test]
    fn repository_id_selection_falls_back_only_for_single_url_candidate() {
        assert_eq!(
            select_repository_id_for_export(&[(7, "D:\\Repos\\TFR Main".to_string())], ""),
            Some(7)
        );
        assert_eq!(
            select_repository_id_for_export(
                &[
                    (7, "D:\\Repos\\TFR Main".to_string()),
                    (8, "E:\\Repos\\TFR Main".to_string())
                ],
                ""
            ),
            None
        );
    }
}
