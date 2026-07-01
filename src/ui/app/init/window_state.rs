use std::fs;

use log::{debug, info};

use crate::ui::app::Foxy;
use crate::ui::types::WindowState;

impl Foxy {
    pub fn load_window_state() -> Option<WindowState> {
        let window_state_path = Self::get_window_state_path();
        match fs::read_to_string(&window_state_path) {
            Ok(json_string) => match serde_json::from_str::<WindowState>(&json_string) {
                Ok(window_state) => {
                    debug!("Loaded window_state.json");
                    Some(window_state)
                }
                Err(err) => {
                    log::error!("Failed to parse window_state.json: {}", err);
                    None
                }
            },
            Err(_) => None,
        }
    }

    pub fn save_window_state(&self, window_state: &WindowState) {
        let window_state_path = Self::get_window_state_path();
        match serde_json::to_string_pretty(window_state) {
            Ok(json_string) => {
                if let Err(err) = crate::core::utils::fs_safety::atomic_write(
                    &window_state_path,
                    json_string.as_bytes(),
                ) {
                    log::error!("Failed to write window_state.json: {}", err);
                } else {
                    debug!("Saved window_state.json");
                }
            }
            Err(err) => log::error!("Failed to serialize window_state.json: {}", err),
        }
    }

    fn current_window_state(&self, ctx: &egui::Context) -> Option<WindowState> {
        let (minimized, maximized, outer_rect) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.minimized.unwrap_or(false),
                viewport.maximized.unwrap_or(false),
                viewport.outer_rect,
            )
        });
        if minimized {
            return None;
        }

        let outer_rect = outer_rect?;
        // `outer_rect` is in egui points, which depend on the current zoom
        // factor (the UI scale). The viewport builder restores geometry in
        // native logical pixels, which are zoom-independent. Convert here so a
        // non-default UI scale does not distort the persisted window geometry.
        let zoom = ctx.zoom_factor();
        let size = outer_rect.size() * zoom;
        let position = [outer_rect.min.x * zoom, outer_rect.min.y * zoom];
        if !position[0].is_finite()
            || !position[1].is_finite()
            || !size.x.is_finite()
            || !size.y.is_finite()
            || size.x < 64.0
            || size.y < 64.0
        {
            return None;
        }

        Some(WindowState {
            position: Some(position),
            size: Some([size.x, size.y]),
            maximized,
        })
    }

    /// Collect the current monitor and app resolutions and log them at startup
    /// and whenever they change (e.g. the window is moved to another display,
    /// resized, or the OS scaling factor changes).
    pub(crate) fn log_display_metrics_if_changed(&mut self, ctx: &egui::Context) {
        let (monitor, app, scale_percent) = ctx.input(|i| {
            let viewport = i.viewport();
            let monitor = viewport
                .monitor_size
                .map(|size| [size.x.round() as i32, size.y.round() as i32]);
            let app = viewport
                .inner_rect
                .map(|rect| rect.size())
                .map(|size| [size.x.round() as i32, size.y.round() as i32]);
            (monitor, app, (i.pixels_per_point * 100.0).round() as i32)
        });

        let metrics = (monitor, app, scale_percent);
        if self.last_logged_display_metrics == Some(metrics) {
            return;
        }
        let first = self.last_logged_display_metrics.is_none();
        self.last_logged_display_metrics = Some(metrics);

        let describe = |resolution: Option<[i32; 2]>| match resolution {
            Some([width, height]) => format!("{width}x{height}"),
            None => "unknown".to_string(),
        };
        let monitor_text = describe(monitor);
        let app_text = describe(app);
        let scale = scale_percent as f32 / 100.0;

        if first {
            info!(
                "Display metrics at startup: monitor resolution {monitor_text}, app resolution {app_text} (scale {scale:.2}x)"
            );
        } else {
            info!(
                "Display metrics changed: monitor resolution {monitor_text}, app resolution {app_text} (scale {scale:.2}x)"
            );
        }
    }

    pub(crate) fn persist_window_state_if_changed(&mut self, ctx: &egui::Context) {
        let Some(window_state) = self.current_window_state(ctx) else {
            return;
        };
        if self.last_saved_window_state.as_ref() == Some(&window_state) {
            return;
        }

        self.save_window_state(&window_state);
        self.last_saved_window_state = Some(window_state);
    }
}
