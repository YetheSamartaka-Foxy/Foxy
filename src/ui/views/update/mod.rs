mod direct_download;
mod manifest;
mod repository_update;

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::i18n::{fmt_bytes, fmt_duration, fmt_duration_ms, fmt_speed_mbps, locale_compare};
use crate::ui::types::FoxyView;
use arboard::Clipboard;
use eframe::egui::{
    self, Align, Align2, Button, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, ProgressBar,
    RichText, ScrollArea, Sense, TextStyle, Ui, Vec2,
};
use log::{info, warn};
use rfd::FileDialog;
use std::time::Duration;
