use std::collections::HashMap;

use eframe::egui;
use eframe::egui::accesskit::{Node, NodeId, Role};

fn bidi_context() -> egui::Context {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "Noto Sans Arabic",
            include_bytes!("../../fonts/NotoSansArabic-Regular.ttf").as_slice(),
        ),
        (
            "Noto Sans Hebrew",
            include_bytes!("../../fonts/NotoSansHebrew-Regular.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), egui::FontData::from_static(bytes).into());
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push(name.to_owned());
    }
    ctx.set_fonts(fonts);
    ctx
}

#[test]
fn bidi_labels_reorder_words_and_keep_digits_and_text_logical() {
    let ctx = bidi_context();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        for (first, second) in [("سلام", "عليكم"), ("שלום", "עולם")] {
            let text = format!("{first} {second} 123 (PDF)");
            let galley = ui.fonts_mut(|fonts| {
                fonts.layout_no_wrap(
                    text.clone(),
                    egui::FontId::proportional(20.0),
                    egui::Color32::WHITE,
                )
            });
            assert_eq!(galley.text(), text);
            assert_eq!(galley.rows.len(), 1);
            let glyphs = &galley.rows[0].glyphs;
            assert_eq!(glyphs.iter().map(|g| g.chr).collect::<String>(), text);
            let second_start = first.chars().count() + 1;
            let digit_start = second_start + second.chars().count() + 1;
            assert!(
                glyphs[..second_start - 1].iter().all(|first_glyph| {
                    glyphs[second_start..digit_start - 1]
                        .iter()
                        .all(|second_glyph| first_glyph.pos.x > second_glyph.pos.x)
                }),
                "{first} must appear to the right of {second}"
            );
            let digits = &glyphs[digit_start..digit_start + 3];
            assert!(digits.windows(2).all(|pair| pair[0].pos.x < pair[1].pos.x));
            let latin = &glyphs[digit_start + 5..digit_start + 8];
            assert!(latin.windows(2).all(|pair| pair[0].pos.x < pair[1].pos.x));
        }
    });
    output.textures_delta.clear();
}

#[test]
fn wrapped_bidi_rows_keep_characters_and_caret_indices() {
    let ctx = bidi_context();
    let text = "بِسْمِ الملف 123 (PDF) مرحبا\nשלום 45 עולם";
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let galley = ui.fonts_mut(|fonts| {
            fonts.layout(
                text.to_owned(),
                egui::FontId::proportional(20.0),
                egui::Color32::WHITE,
                100.0,
            )
        });
        assert!(galley.rows.len() > 2, "fixture must wrap");
        let mut recovered = String::new();
        for row in &galley.rows {
            recovered.extend(row.glyphs.iter().map(|glyph| glyph.chr));
            if row.ends_with_newline {
                recovered.push('\n');
            }
        }
        assert_eq!(recovered, text);

        let total = text.chars().count();
        for index in 0..=total {
            let position = galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .center();
            assert!(position.is_finite(), "invalid caret for {index}");
            assert!(galley.cursor_from_pos(position.to_vec2()).index.0 <= total);
        }
        let arabic_offset = text.find("الملف").expect("Arabic word");
        let arabic_start = text[..arabic_offset].chars().count();
        let digit_offset = text.find("123").expect("digit run");
        let digit_start = text[..digit_offset].chars().count();
        let arabic_interior = arabic_start + 1..arabic_start + "الملف".chars().count();
        for index in arabic_interior.chain(digit_start + 1..digit_start + 3) {
            let position = galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .center();
            assert_eq!(galley.cursor_from_pos(position.to_vec2()).index.0, index);
        }
        // The two ends of a directional run can share the same caret position.
        let digit_end = digit_start + 3;
        let position = galley
            .pos_from_cursor(egui::text::CCursor::new(digit_end))
            .center();
        let hit = galley.cursor_from_pos(position.to_vec2()).index.0;
        assert!(hit == digit_start || hit == digit_end);
    });
    output.textures_delta.clear();
}

fn assert_accesskit_text(tree: &HashMap<NodeId, Node>, widget: NodeId, expected: &str) -> usize {
    let widget = tree.get(&widget).expect("widget node");
    let runs: Vec<_> = widget
        .children()
        .iter()
        .filter_map(|id| tree.get(id))
        .filter(|node| node.role() == Role::TextRun)
        .collect();
    let value: String = runs.iter().filter_map(|node| node.value()).collect();
    let chars = runs
        .iter()
        .map(|node| node.character_lengths().len())
        .sum::<usize>();
    assert_eq!(value, expected);
    assert_eq!(chars, expected.chars().count());
    runs.len()
}

#[test]
fn bidi_accesskit_label_and_text_edit_expose_logical_text_without_duplicates() {
    let ctx = bidi_context();
    ctx.enable_accesskit();
    let label = "مرحبا 123 (PDF) שלום";
    let mut single_line = "שלום 12 سلام PDF ".repeat(24);
    let mut multiline = "بِسْمِ الملف 123\nשלום 45 עולם".to_owned();
    let mut nodes = HashMap::new();
    let mut widgets = None;
    for _ in 0..2 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let label_id = ui.add(egui::Label::new(label).selectable(true)).id;
            let edit_id = ui.text_edit_singleline(&mut single_line).id;
            let multi_id = ui.text_edit_multiline(&mut multiline).id;
            widgets = Some((label_id, edit_id, multi_id));
        });
        output.textures_delta.clear();
        nodes.extend(
            output
                .platform_output
                .accesskit_update
                .expect("text tree")
                .nodes,
        );
    }

    let (label_id, edit_id, multi_id) = widgets.expect("text widgets");
    assert_accesskit_text(&nodes, label_id.accesskit_id(), label);
    assert!(assert_accesskit_text(&nodes, edit_id.accesskit_id(), &single_line) > 1);
    assert!(assert_accesskit_text(&nodes, multi_id.accesskit_id(), &multiline) > 1);
}
