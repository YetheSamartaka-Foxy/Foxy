use eframe::egui;
use eframe::egui::Ui;

pub(super) fn blend_header_color(
    start: egui::Color32,
    end: egui::Color32,
    factor: f32,
) -> egui::Color32 {
    let factor = factor.clamp(0.0, 1.0);
    let blend = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * factor).round() as u8 };
    egui::Color32::from_rgba_unmultiplied(
        blend(start.r(), end.r()),
        blend(start.g(), end.g()),
        blend(start.b(), end.b()),
        blend(start.a(), end.a()),
    )
}

pub(super) fn paint_header_gradient(ui: &Ui, rect: egui::Rect, stops: &[(f32, egui::Color32)]) {
    if rect.is_negative() || stops.len() < 2 {
        return;
    }

    let mut mesh = egui::Mesh::default();
    for (stop, color) in stops {
        let x = rect.left() + rect.width() * stop.clamp(0.0, 1.0);
        mesh.colored_vertex(egui::pos2(x, rect.top()), *color);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), *color);
    }

    for idx in 0..(stops.len() - 1) {
        let left = (idx * 2) as u32;
        mesh.add_triangle(left, left + 1, left + 3);
        mesh.add_triangle(left, left + 3, left + 2);
    }

    ui.painter().add(egui::Shape::mesh(mesh));
}
