mod direct_download;
mod manifest;
mod repository_update;

use crate::core::api::{FileDiffKind, ModDiffSummary, SyncMode};
use crate::core::utils::disk_space::DiskSpaceShortfall;
use crate::ui::app::{ActivityBar, BarMotion, Foxy, transfer_progress};
use crate::ui::i18n::{
    fmt_bytes, fmt_bytes_per_sec, fmt_duration, fmt_duration_ms, fmt_speed_mbps, locale_compare,
};
use crate::ui::types::FoxyView;
use arboard::Clipboard;
use eframe::egui::{
    self, Align, Align2, Button, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, RichText,
    ScrollArea, Sense, TextStyle, Ui, Vec2,
};
use log::{info, warn};
use rfd::FileDialog;
use std::hash::{Hash, Hasher};
use std::time::Duration;

/// How a download bar moves: forward while transferring, held while paused,
/// backward while a cancel rolls changes back.
fn update_bar_motion(transferring: bool, paused: bool, cancelling: bool) -> BarMotion {
    if !transferring {
        BarMotion::Still
    } else if cancelling {
        BarMotion::Backward
    } else if paused {
        BarMotion::Held
    } else {
        BarMotion::Forward
    }
}
