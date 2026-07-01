mod polling;
mod update_view;
mod version_browser;

use crate::core::tasks::app_update::{
    self, AppUpdateEvent, ChangelogVersion, UpdateCheckStatus, VersionEntry,
};
use crate::ui::app::Foxy;
use eframe::egui;
use eframe::egui::{
    Align, Button, CursorIcon, Frame, Label, Layout, Margin, ProgressBar, RichText, ScrollArea, Ui,
    Vec2,
};

const BYTES_PER_MB: f64 = 1_048_576.0;

fn format_mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / BYTES_PER_MB)
}
