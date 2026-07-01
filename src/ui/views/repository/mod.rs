mod banner_translations;
mod banners;
mod launch;
mod list;
mod list_rows;
mod mission_cards;
mod modals;
mod navigation;
mod server_cards;
mod space_cards;
mod space_settings;
mod spaces;
mod view;

use crate::ui::app::Foxy;
use eframe::egui::Color32;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

pub(super) struct RepositoryCheckStatusBanner {
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) hint: String,
    pub(super) progress: Option<f32>,
    pub(super) elapsed_seconds: u64,
}

pub(super) struct RepositoryCheckCompletionBanner {
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) stroke_color: Color32,
    pub(super) show_pending_action: bool,
}

pub(super) struct RepositoryActionBanner {
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) stroke_color: Color32,
    pub(super) button_label: String,
    pub(super) button_fill: Color32,
    pub(super) action: RepositoryActionBannerAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositoryActionBannerAction {
    PendingUpdate,
    UpdateView,
    UpdateSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositoryBannerResponse {
    None,
    ActionClicked,
    DismissClicked,
}

pub(super) type RepositoryUiAction = Box<dyn FnOnce(&mut Foxy)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaunchDispatchResult {
    Launched,
    Deferred,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositoryListSectionContextAction {
    ToggleCollapsed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositorySpaceRowContextAction {
    ToggleCollapsed,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositoryServerContextAction {
    RefreshStatus,
    Join,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositoryMissionContextAction {
    OpenFolder,
    OpenInEditor,
    RemoveDependencies,
    Duplicate,
    Delete,
}

pub(super) fn spawn_launch_process(
    executable: &OsString,
    args: &[OsString],
    cwd: Option<&Path>,
) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new(executable);
    cmd.args(args);
    if let Some(working_dir) = cwd {
        cmd.current_dir(working_dir);
    }
    cmd.spawn()
}

pub(super) fn arma3_editor_display_name(raw: &str) -> String {
    repair_utf8_mojibake(&percent_decode_utf8(raw))
}

fn percent_decode_utf8(raw: &str) -> String {
    if !raw.contains('%') {
        return raw.to_string();
    }

    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                hex_value(bytes.get(index + 1)),
                hex_value(bytes.get(index + 2)),
            )
        {
            decoded.push(high << 4 | low);
            index += 3;
            continue;
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).unwrap_or_else(|_| raw.to_string())
}

fn hex_value(byte: Option<&u8>) -> Option<u8> {
    let byte = byte.copied()?;
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn repair_utf8_mojibake(raw: &str) -> String {
    if !raw.contains(['\u{00c3}', '\u{00c5}', '\u{00c4}', '\u{00c2}']) {
        return raw.to_string();
    }

    let Some(latin1_bytes) = raw
        .chars()
        .map(|ch| u8::try_from(u32::from(ch)).ok())
        .collect::<Option<Vec<_>>>()
    else {
        return raw.to_string();
    };

    String::from_utf8(latin1_bytes).unwrap_or_else(|_| raw.to_string())
}

impl Foxy {
    fn format_compact_elapsed_duration(duration: Duration) -> String {
        if duration.as_secs() == 0 {
            return format!("{} ms", duration.as_millis());
        }

        let total_secs = duration.as_secs();
        let days = total_secs / 86_400;
        let hours = (total_secs % 86_400) / 3_600;
        let minutes = (total_secs % 3_600) / 60;
        let seconds = total_secs % 60;

        if days > 0 {
            format!("{}d {}h {}m", days, hours, minutes)
        } else if hours > 0 {
            format!("{}h {}m {}s", hours, minutes, seconds)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else if duration.as_secs_f64() < 10.0 {
            format!("{:.1} s", duration.as_secs_f64())
        } else {
            format!("{} s", total_secs)
        }
    }

    fn repository_list_row_height() -> f32 {
        40.0
    }

    pub(super) fn truncate_display_name(name: &str, max_chars: usize) -> String {
        if name.chars().count() > max_chars {
            format!("{}…", name.chars().take(max_chars).collect::<String>())
        } else {
            name.to_string()
        }
    }

    fn repository_space_selector_entry_row_height() -> f32 {
        72.0
    }

    fn repository_space_candidate_row_height() -> f32 {
        28.0
    }

    fn repository_list_section_row_height() -> f32 {
        38.0
    }
}

#[cfg(test)]
mod tests {
    use super::arma3_editor_display_name;

    #[test]
    fn editor_display_name_decodes_utf8_percent_sequences() {
        assert_eq!(
            arma3_editor_display_name("Na%20slun%c3%ad%c4%8dku"),
            "Na slun\u{00ed}\u{010d}ku"
        );
    }

    #[test]
    fn editor_display_name_repairs_legacy_profile_mojibake() {
        assert_eq!(
            arma3_editor_display_name("Li\u{00c5}\u{00a1}ka137"),
            "Li\u{0161}ka137"
        );
    }

    #[test]
    fn editor_display_name_keeps_plain_names_unchanged() {
        assert_eq!(
            arma3_editor_display_name("Simple Mission"),
            "Simple Mission"
        );
    }
}
