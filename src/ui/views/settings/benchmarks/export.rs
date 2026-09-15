//! ZIP export of a benchmark, with the charts captured as PNG through
//! viewport screenshots of a chart window, one chart per pass.

use std::io::Cursor;

use eframe::egui::{self, Event, UserData, ViewportCommand};
use log::{info, warn};

use crate::core::benchmarks::export::ExportImage;
use crate::ui::app::Foxy;
use crate::ui::app::benchmarks::BenchmarkExportJob;

const EXPORT_WINDOW_WIDTH: f32 = 900.0;
const EXPORT_CHART_HEIGHT: f32 = 260.0;
/// Frames the window gets to settle before each capture (no fade-in, but the
/// first frame only measures the content).
const SETTLE_FRAMES: u32 = 3;
/// Frames to wait for a screenshot before giving up on that chart, so a
/// backend that never answers cannot leave the export hanging.
const SCREENSHOT_TIMEOUT_FRAMES: u32 = 180;

fn encode_png(image: &egui::ColorImage) -> Result<Vec<u8>, String> {
    let mut buffer = Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut buffer,
        image.as_raw(),
        image.size[0] as u32,
        image.size[1] as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|err| err.to_string())?;
    Ok(buffer.into_inner())
}

impl Foxy {
    pub(crate) fn start_benchmark_export(&mut self, id: &str) {
        if self.benchmarks_view.export.is_some() {
            return;
        }
        let Some(record) = self.benchmarks_view.record(id) else {
            return;
        };
        let file_name = format!("{}.zip", record.id);
        let chart_count = Self::benchmark_chart_count(record);
        let dest = crate::ui::app::agent_support::save_file(|| {
            rfd::FileDialog::new()
                .set_file_name(&file_name)
                .add_filter("ZIP archive", &["zip"])
                .save_file()
        });
        let Some(dest) = dest else {
            return;
        };
        info!("Benchmark export started: id={id} charts={chart_count}");
        self.benchmarks_view.export = Some(BenchmarkExportJob {
            id: id.to_owned(),
            dest,
            chart_index: 0,
            chart_count,
            frames_rendered: 0,
            screenshot_requested: false,
            chart_rect: None,
            images: Vec::new(),
        });
    }

    fn finish_benchmark_export(&mut self) {
        if let Some(job) = self.benchmarks_view.export.take() {
            self.write_benchmark_export(&job.id, job.dest, job.images);
        }
    }

    /// Drives a running export: renders the current chart, requests the
    /// screenshot once the window has settled, crops the chart when it
    /// arrives, then moves to the next chart.
    pub(crate) fn poll_benchmark_export_screenshot(&mut self, ctx: &egui::Context) {
        let Some(job) = self.benchmarks_view.export.as_ref() else {
            return;
        };
        let (id, chart_index, chart_count, screenshot_requested, frames_rendered) = (
            job.id.clone(),
            job.chart_index,
            job.chart_count,
            job.screenshot_requested,
            job.frames_rendered,
        );
        let Some(record) = self.benchmarks_view.record(&id).cloned() else {
            self.benchmarks_view.export = None;
            return;
        };
        if chart_index >= chart_count {
            self.finish_benchmark_export();
            return;
        }

        if screenshot_requested {
            let events = ctx.input(|input| input.events.clone());
            let shot = events.into_iter().find_map(|event| match event {
                Event::Screenshot {
                    user_data, image, ..
                } if user_data
                    .data
                    .as_ref()
                    .and_then(|data| data.downcast_ref::<String>())
                    .is_some_and(|known| *known == id) =>
                {
                    Some(image)
                }
                _ => None,
            });
            let timed_out = frames_rendered > SCREENSHOT_TIMEOUT_FRAMES;
            if shot.is_some() || timed_out {
                let ppp = ctx.pixels_per_point();
                if let (Some(image), Some(job)) = (shot, self.benchmarks_view.export.as_mut())
                    && let Some((key, rect)) = job.chart_rect.take()
                {
                    let bounds = egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::Vec2::new(image.size[0] as f32 / ppp, image.size[1] as f32 / ppp),
                    );
                    let rect = rect.intersect(bounds);
                    if rect.width() >= 4.0 && rect.height() >= 4.0 {
                        match encode_png(&image.region(&rect, Some(ppp))) {
                            Ok(png) => job.images.push(ExportImage {
                                file_name: format!("{key}.png"),
                                png,
                            }),
                            Err(err) => warn!("Benchmark chart {key} could not be encoded: {err}"),
                        }
                    }
                } else if timed_out {
                    warn!("Benchmark export: no screenshot arrived for chart {chart_index}");
                }
                if let Some(job) = self.benchmarks_view.export.as_mut() {
                    job.chart_index += 1;
                    job.frames_rendered = 0;
                    job.screenshot_requested = false;
                    job.chart_rect = None;
                }
                if chart_index + 1 >= chart_count {
                    self.finish_benchmark_export();
                    return;
                }
            }
        }

        let frame = self
            .modal_window_chrome(ctx)
            .fill(self.color_card_bg().to_opaque());
        let mut rects = Vec::new();
        egui::Window::new(self.t("Exporting benchmark"))
            .id(egui::Id::new("benchmark_export_window"))
            // No fade-in: the screenshot must not catch a half-transparent window.
            .fade_in(false)
            .frame(frame)
            .title_frame(self.modal_window_chrome(ctx))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([EXPORT_WINDOW_WIDTH, 0.0])
            .show(ctx, |ui| {
                ui.set_width(EXPORT_WINDOW_WIDTH);
                ui.label(format!(
                    "{}  ({}/{})",
                    record.name,
                    chart_index + 1,
                    chart_count
                ));
                rects = self.render_benchmark_charts(
                    ui,
                    &record,
                    EXPORT_CHART_HEIGHT,
                    Some(chart_index),
                );
            });
        if let Some(job) = self.benchmarks_view.export.as_mut() {
            job.frames_rendered += 1;
            job.chart_rect = rects.into_iter().next();
            if !job.screenshot_requested && job.frames_rendered >= SETTLE_FRAMES {
                job.screenshot_requested = true;
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::new(job.id.clone())));
            }
        }
        ctx.request_repaint();
    }
}
