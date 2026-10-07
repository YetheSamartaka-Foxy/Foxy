//! Small immediate-mode charts for benchmark series: a time line chart with
//! area fill and hover readout, and a horizontal bar chart for stage
//! durations. Painted with the egui painter so the same code renders on
//! screen and into the export screenshot.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, TextStyle, Ui, Vec2,
    epaint::{Mesh, Vertex, WHITE_UV},
};

use crate::ui::i18n::fmt_bytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YUnit {
    Bytes,
    BytesPerSec,
    Percent,
    Count,
}

pub fn fmt_y(unit: YUnit, value: f64) -> String {
    match unit {
        YUnit::Bytes => fmt_bytes(value.max(0.0) as u64),
        YUnit::BytesPerSec => format!("{}/s", fmt_bytes(value.max(0.0) as u64)),
        YUnit::Percent => format!("{value:.0}%"),
        YUnit::Count => format!("{value:.0}"),
    }
}

/// A duration in the unit that keeps it readable: milliseconds under a
/// second, seconds under a minute, else `m:ss`.
pub fn fmt_duration(secs: f64) -> String {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    if secs < 0.0005 {
        "0 ms".to_owned()
    } else if secs < 0.01 {
        format!("{:.1} ms", secs * 1000.0)
    } else if secs < 1.0 {
        format!("{:.0} ms", secs * 1000.0)
    } else if secs < 60.0 {
        format!("{secs:.2} s")
    } else {
        let total = secs.round() as u64;
        format!("{}:{:02} min", total / 60, total % 60)
    }
}

pub fn fmt_secs(secs: f64) -> String {
    if secs < 10.0 {
        return format!("{secs:.1}s");
    }
    let total = secs.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Clone, Debug)]
pub struct ChartSeries {
    pub label: String,
    pub color: Color32,
    /// `[seconds, value]`, sorted by seconds.
    pub points: Vec<[f64; 2]>,
    /// A running total rather than a rate: the mean is meaningless, only
    /// the final value is reported.
    pub cumulative: bool,
}

impl ChartSeries {
    pub fn has_signal(&self) -> bool {
        self.points.iter().any(|point| point[1] > 0.0)
    }

    /// Mean and maximum of the values, `None` for an empty series.
    pub fn stats(&self) -> Option<(f64, f64)> {
        if self.points.is_empty() {
            return None;
        }
        let sum: f64 = self.points.iter().map(|point| point[1]).sum();
        let peak = self
            .points
            .iter()
            .map(|point| point[1])
            .fold(0.0_f64, f64::max);
        Some((sum / self.points.len() as f64, peak))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ChartTheme {
    pub background: Color32,
    pub plot: Color32,
    pub grid: Color32,
    pub axis: Color32,
    pub text: Color32,
    pub muted: Color32,
}

const LEFT_MARGIN: f32 = 64.0;
const RIGHT_MARGIN: f32 = 12.0;
const TOP_MARGIN: f32 = 36.0;
const BOTTOM_MARGIN: f32 = 22.0;
const AREA_ALPHA: u8 = 44;

fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn chart_fonts(ui: &Ui) -> (FontId, FontId) {
    let body = TextStyle::Body.resolve(ui.style());
    let small = FontId::new((body.size * 0.8).max(10.0), body.family.clone());
    (body, small)
}

/// A "nice" axis maximum a little above `max` so the top gridline has a
/// round label.
pub fn nice_axis_max(max: f64) -> f64 {
    if max <= 0.0 || !max.is_finite() {
        return 1.0;
    }
    let magnitude = 10f64.powf(max.log10().floor());
    let scaled = max / magnitude;
    let step = if scaled <= 1.0 {
        1.0
    } else if scaled <= 2.0 {
        2.0
    } else if scaled <= 2.5 {
        2.5
    } else if scaled <= 5.0 {
        5.0
    } else {
        10.0
    };
    step * magnitude
}

/// `nice_axis_max` in the unit the axis labels use, so byte axes land on
/// round binary multiples (5 GB rather than 4.66 GB).
pub fn nice_axis_max_for(unit: YUnit, max: f64) -> f64 {
    match unit {
        YUnit::Bytes | YUnit::BytesPerSec if max >= 1024.0 => {
            let scale = 1024f64.powi(max.log(1024.0).floor() as i32);
            nice_axis_max(max / scale) * scale
        }
        _ => nice_axis_max(max),
    }
}

/// Value of `series` at time `secs`, taken from the nearest sample.
pub fn value_at(series: &ChartSeries, secs: f64) -> Option<f64> {
    if series.points.is_empty() {
        return None;
    }
    let index = series
        .points
        .partition_point(|point| point[0] < secs)
        .min(series.points.len() - 1);
    let candidate = if index > 0
        && (series.points[index - 1][0] - secs).abs() < (series.points[index][0] - secs).abs()
    {
        index - 1
    } else {
        index
    };
    Some(series.points[candidate][1])
}

/// Translucent band between the series line and the plot floor, built as a
/// triangle strip so concave curves fill correctly.
fn area_mesh(points: &[Pos2], floor: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    for point in points {
        mesh.vertices.push(Vertex {
            pos: *point,
            uv: WHITE_UV,
            color,
        });
        mesh.vertices.push(Vertex {
            pos: Pos2::new(point.x, floor),
            uv: WHITE_UV,
            color,
        });
    }
    for index in 1..points.len() as u32 {
        let (top, bottom) = (index * 2, index * 2 + 1);
        let (prev_top, prev_bottom) = (top - 2, bottom - 2);
        mesh.add_triangle(prev_top, prev_bottom, top);
        mesh.add_triangle(prev_bottom, bottom, top);
    }
    mesh
}

fn paint_legend(
    painter: &egui::Painter,
    right: f32,
    top: f32,
    entries: impl DoubleEndedIterator<Item = (String, Color32)>,
    font: &FontId,
    color: Color32,
) {
    let mut legend_x = right;
    for (label, swatch_color) in entries.rev() {
        let label_rect = painter.text(
            Pos2::new(legend_x, top),
            Align2::RIGHT_TOP,
            label,
            font.clone(),
            color,
        );
        let swatch = Rect::from_center_size(
            Pos2::new(label_rect.min.x - 9.0, label_rect.center().y),
            Vec2::splat(8.0),
        );
        painter.rect_filled(swatch, 2.0, swatch_color);
        legend_x = swatch.min.x - 12.0;
    }
}

pub fn line_chart(
    ui: &mut Ui,
    height: f32,
    title: &str,
    unit: YUnit,
    series: &[ChartSeries],
    theme: ChartTheme,
) -> Rect {
    let width = ui.available_width().max(120.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let response = response
        .with_new_rect(rect)
        .on_hover_cursor(egui::CursorIcon::Crosshair);
    let painter = ui.painter_at(rect);
    let (font_body, font_small) = chart_fonts(ui);
    painter.rect_filled(rect, 6.0, theme.background);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, theme.grid),
        egui::StrokeKind::Inside,
    );
    let plot = Rect::from_min_max(
        rect.min + Vec2::new(LEFT_MARGIN, TOP_MARGIN),
        rect.max - Vec2::new(RIGHT_MARGIN, BOTTOM_MARGIN),
    );
    painter.rect_filled(plot, 3.0, theme.plot);

    let title_rect = painter.text(
        rect.min + Vec2::new(10.0, 8.0),
        Align2::LEFT_TOP,
        title,
        font_body.clone(),
        theme.text,
    );
    if let [single] = series
        && let Some((avg, peak)) = single.stats()
    {
        let summary = if single.cumulative {
            format!("total {}", fmt_y(unit, peak))
        } else {
            format!("avg {}  peak {}", fmt_y(unit, avg), fmt_y(unit, peak))
        };
        painter.text(
            Pos2::new(title_rect.max.x + 10.0, title_rect.center().y),
            Align2::LEFT_CENTER,
            summary,
            font_small.clone(),
            theme.muted,
        );
    }
    paint_legend(
        &painter,
        rect.max.x - RIGHT_MARGIN,
        rect.min.y + 9.0,
        series
            .iter()
            .map(|entry| (entry.label.clone(), entry.color)),
        &font_small,
        theme.text,
    );

    let x_max = series
        .iter()
        .flat_map(|s| s.points.last().map(|p| p[0]))
        .fold(1.0_f64, f64::max);
    let y_max = nice_axis_max_for(
        unit,
        series
            .iter()
            .flat_map(|s| s.points.iter().map(|p| p[1]))
            .fold(0.0_f64, f64::max),
    );
    let to_screen = |x: f64, y: f64| -> Pos2 {
        Pos2::new(
            plot.min.x + ((x / x_max).clamp(0.0, 1.0) as f32) * plot.width(),
            plot.max.y - ((y / y_max).clamp(0.0, 1.0) as f32) * plot.height(),
        )
    };

    for step in 0..=4 {
        let fraction = step as f64 / 4.0;
        let y = plot.max.y - fraction as f32 * plot.height();
        if step > 0 {
            painter.line_segment(
                [Pos2::new(plot.min.x, y), Pos2::new(plot.max.x, y)],
                Stroke::new(1.0, theme.grid),
            );
        }
        painter.text(
            Pos2::new(plot.min.x - 6.0, y),
            Align2::RIGHT_CENTER,
            fmt_y(unit, y_max * fraction),
            font_small.clone(),
            theme.muted,
        );
    }
    for step in 1..4 {
        let x = plot.min.x + step as f32 / 4.0 * plot.width();
        painter.line_segment(
            [Pos2::new(x, plot.min.y), Pos2::new(x, plot.max.y)],
            Stroke::new(1.0, theme.grid),
        );
    }
    for step in 0..=4 {
        let fraction = step as f64 / 4.0;
        let x = plot.min.x + fraction as f32 * plot.width();
        let align = match step {
            0 => Align2::LEFT_TOP,
            4 => Align2::RIGHT_TOP,
            _ => Align2::CENTER_TOP,
        };
        painter.text(
            Pos2::new(x, plot.max.y + 4.0),
            align,
            fmt_secs(x_max * fraction),
            font_small.clone(),
            theme.muted,
        );
    }
    painter.line_segment(
        [plot.left_bottom(), plot.right_bottom()],
        Stroke::new(1.0, theme.axis),
    );
    painter.line_segment(
        [plot.left_top(), plot.left_bottom()],
        Stroke::new(1.0, theme.axis),
    );

    for entry in series {
        let points: Vec<Pos2> = entry.points.iter().map(|p| to_screen(p[0], p[1])).collect();
        match points.as_slice() {
            [] => {}
            [only] => {
                painter.circle_filled(*only, 3.0, entry.color);
            }
            _ => {
                painter.add(Shape::mesh(area_mesh(
                    &points,
                    plot.max.y,
                    with_alpha(entry.color, AREA_ALPHA),
                )));
                painter.add(Shape::line(points.clone(), Stroke::new(2.0, entry.color)));
                if let Some(last) = points.last() {
                    painter.circle_filled(*last, 3.0, entry.color);
                }
            }
        }
    }

    if let Some(pointer) = response.hover_pos().filter(|pos| plot.contains(*pos)) {
        let secs = ((pointer.x - plot.min.x) / plot.width()) as f64 * x_max;
        painter.line_segment(
            [
                Pos2::new(pointer.x, plot.min.y),
                Pos2::new(pointer.x, plot.max.y),
            ],
            Stroke::new(1.0, theme.axis),
        );
        let mut lines = vec![fmt_secs(secs)];
        for entry in series {
            if let Some(value) = value_at(entry, secs) {
                let marker = to_screen(secs, value);
                painter.circle_filled(marker, 4.5, theme.plot);
                painter.circle_filled(marker, 3.0, entry.color);
                lines.push(format!("{}: {}", entry.label, fmt_y(unit, value)));
            }
        }
        let text = lines.join("\n");
        let galley = painter.layout_no_wrap(text, font_small, theme.text);
        let size = galley.size() + Vec2::splat(12.0);
        let mut anchor = pointer + Vec2::new(14.0, -size.y / 2.0);
        if anchor.x + size.x > rect.max.x {
            anchor.x = pointer.x - 14.0 - size.x;
        }
        anchor.y = anchor
            .y
            .clamp(rect.min.y, (rect.max.y - size.y).max(rect.min.y));
        let tooltip = Rect::from_min_size(anchor, size);
        painter.rect_filled(tooltip, 4.0, theme.background);
        painter.rect_stroke(
            tooltip,
            4.0,
            Stroke::new(1.0, theme.axis),
            egui::StrokeKind::Inside,
        );
        painter.galley(tooltip.min + Vec2::splat(6.0), galley, theme.text);
    }
    rect
}

#[derive(Clone, Debug)]
pub struct BarRow {
    pub label: String,
    /// One value per compared record, in legend order.
    pub values: Vec<f64>,
}

/// Share of `value` in the total of column `index` across `rows`, as a
/// percentage; `None` when that column sums to zero.
pub fn bar_share(rows: &[BarRow], index: usize, value: f64) -> Option<f64> {
    let total: f64 = rows
        .iter()
        .map(|row| row.values.get(index).copied().unwrap_or(0.0))
        .sum();
    (total > 0.0).then(|| value / total * 100.0)
}

pub fn bar_chart(
    ui: &mut Ui,
    title: &str,
    rows: &[BarRow],
    legend: &[(String, Color32)],
    theme: ChartTheme,
) -> Rect {
    const BAR_HEIGHT: f32 = 14.0;
    const ROW_GAP: f32 = 7.0;
    const LABEL_WIDTH: f32 = 168.0;
    const VALUE_WIDTH: f32 = 92.0;
    let bars_per_row = legend.len().max(1);
    let row_height = BAR_HEIGHT * bars_per_row as f32 + ROW_GAP;
    let height = TOP_MARGIN + 6.0 + rows.len() as f32 * row_height + 6.0;
    let width = ui.available_width().max(120.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let (font_body, font_small) = chart_fonts(ui);
    painter.rect_filled(rect, 6.0, theme.background);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, theme.grid),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.min + Vec2::new(10.0, 8.0),
        Align2::LEFT_TOP,
        title,
        font_body,
        theme.text,
    );
    paint_legend(
        &painter,
        rect.max.x - RIGHT_MARGIN,
        rect.min.y + 9.0,
        legend.iter().cloned(),
        &font_small,
        theme.text,
    );
    let max = rows
        .iter()
        .flat_map(|row| row.values.iter().copied())
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let bar_left = rect.min.x + 10.0 + LABEL_WIDTH;
    let bar_span = (rect.max.x - RIGHT_MARGIN - VALUE_WIDTH - bar_left).max(20.0);
    let mut y = rect.min.y + TOP_MARGIN + 6.0;
    for row in rows {
        painter.text(
            Pos2::new(
                rect.min.x + 10.0,
                y + (BAR_HEIGHT * bars_per_row as f32) / 2.0,
            ),
            Align2::LEFT_CENTER,
            &row.label,
            font_small.clone(),
            theme.text,
        );
        for (index, (_, color)) in legend.iter().enumerate() {
            let value = row.values.get(index).copied().unwrap_or(0.0);
            let bar_y = y + index as f32 * BAR_HEIGHT;
            let track = Rect::from_min_size(
                Pos2::new(bar_left, bar_y + 2.0),
                Vec2::new(bar_span, BAR_HEIGHT - 4.0),
            );
            painter.rect_filled(track, 3.0, theme.plot);
            let bar = Rect::from_min_size(
                track.min,
                Vec2::new(
                    (((value / max) as f32) * bar_span).max(if value > 0.0 { 2.0 } else { 0.0 }),
                    track.height(),
                ),
            );
            if value > 0.0 {
                painter.rect_filled(bar, 3.0, *color);
            }
            let share = bar_share(rows, index, value)
                .filter(|share| bars_per_row == 1 && *share >= 0.5)
                .map(|share| format!("  {share:.0}%"))
                .unwrap_or_default();
            painter.text(
                Pos2::new(track.max.x + 8.0, track.center().y),
                Align2::LEFT_CENTER,
                format!("{}{share}", fmt_duration(value)),
                font_small.clone(),
                theme.muted,
            );
        }
        y += row_height;
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_picks_a_readable_unit() {
        assert_eq!(fmt_duration(0.0), "0 ms");
        assert_eq!(fmt_duration(0.0004), "0 ms");
        assert_eq!(fmt_duration(0.003), "3.0 ms");
        assert_eq!(fmt_duration(0.087), "87 ms");
        assert_eq!(fmt_duration(0.87), "870 ms");
        assert_eq!(fmt_duration(39.96), "39.96 s");
        assert_eq!(fmt_duration(612.0), "10:12 min");
    }

    #[test]
    fn axis_max_rounds_up_to_round_numbers() {
        assert_eq!(nice_axis_max(0.0), 1.0);
        assert_eq!(nice_axis_max(3.0), 5.0);
        assert_eq!(nice_axis_max(17.0), 20.0);
        assert_eq!(nice_axis_max(240.0), 250.0);
        assert_eq!(nice_axis_max(5_000_000.0), 5_000_000.0);
        assert_eq!(nice_axis_max(7_500_000.0), 10_000_000.0);
    }

    #[test]
    fn value_at_uses_nearest_sample() {
        let series = ChartSeries {
            label: "s".into(),
            color: Color32::WHITE,
            points: vec![[0.0, 1.0], [10.0, 2.0], [20.0, 3.0]],
            cumulative: false,
        };
        assert_eq!(value_at(&series, 4.0), Some(1.0));
        assert_eq!(value_at(&series, 6.0), Some(2.0));
        assert_eq!(value_at(&series, 25.0), Some(3.0));
        assert_eq!(
            value_at(
                &ChartSeries {
                    label: "e".into(),
                    color: Color32::WHITE,
                    points: vec![],
                    cumulative: false,
                },
                1.0
            ),
            None
        );
    }

    #[test]
    fn byte_axes_round_to_binary_multiples() {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        const MIB: f64 = 1024.0 * 1024.0;
        assert_eq!(nice_axis_max_for(YUnit::Bytes, 4.03 * GIB), 5.0 * GIB);
        assert_eq!(
            nice_axis_max_for(YUnit::BytesPerSec, 119.3 * MIB),
            200.0 * MIB
        );
        assert_eq!(nice_axis_max_for(YUnit::Bytes, 500.0), 500.0);
        assert_eq!(nice_axis_max_for(YUnit::Percent, 87.0), 100.0);
    }

    #[test]
    fn series_stats_are_mean_and_peak() {
        let series = ChartSeries {
            label: "s".into(),
            color: Color32::WHITE,
            points: vec![[0.0, 1.0], [1.0, 3.0], [2.0, 2.0]],
            cumulative: false,
        };
        assert_eq!(series.stats(), Some((2.0, 3.0)));
        assert_eq!(
            ChartSeries {
                label: "e".into(),
                color: Color32::WHITE,
                points: vec![],
                cumulative: false,
            }
            .stats(),
            None
        );
    }

    #[test]
    fn area_mesh_covers_every_segment() {
        let points = [
            Pos2::new(0.0, 5.0),
            Pos2::new(1.0, 2.0),
            Pos2::new(2.0, 8.0),
        ];
        let mesh = area_mesh(&points, 10.0, Color32::WHITE);
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 2 * 2 * 3);
        assert!(mesh.vertices.iter().all(|vertex| vertex.pos.y <= 10.0));
    }

    #[test]
    fn bar_share_is_percent_of_column_total() {
        let rows = vec![
            BarRow {
                label: "a".into(),
                values: vec![3.0, 0.0],
            },
            BarRow {
                label: "b".into(),
                values: vec![1.0, 0.0],
            },
        ];
        assert_eq!(bar_share(&rows, 0, 3.0), Some(75.0));
        assert_eq!(bar_share(&rows, 1, 0.0), None);
    }

    #[test]
    fn formats_seconds_as_clock() {
        assert_eq!(fmt_secs(5.0), "5.0s");
        assert_eq!(fmt_secs(10.0), "0:10");
        assert_eq!(fmt_secs(125.0), "2:05");
        assert_eq!(fmt_secs(3725.0), "1:02:05");
    }
}
