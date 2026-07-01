use crate::ui::app::Foxy;
use crate::ui::fonts::AboutViewFonts;
use crate::ui::types::AboutTab;
use eframe::egui::{
    Align, Button, Color32, CursorIcon, FontId, Frame, Hyperlink, Label, Layout, Margin, RichText,
    ScrollArea, TextWrapMode, Ui, Vec2,
    text::{LayoutJob, TextFormat},
};
use log::info;

const EMBEDDED_ABOUT: &str = include_str!("../../../README.md");
/// First-party project license, shown under the License tab.
const EMBEDDED_LICENSE: &str = include_str!("../../../LICENSE");
/// Human-readable licensing/redistribution overview, shown under the Licensing tab.
const EMBEDDED_LICENSING: &str = include_str!("../../../LICENSING.md");
/// Generated third-party dependency notices, shown under the Third-party tab.
const EMBEDDED_THIRD_PARTY: &str = include_str!("../../../THIRD-PARTY-LICENSES.txt");

#[derive(Clone)]
struct AboutInline {
    text: String,
    strong: bool,
}

/// Pre-parsed element from the embedded about markdown.
#[derive(Clone)]
enum AboutElement {
    Heading1(String),
    Heading2(String),
    Heading3(String),
    ListItemLink { label: String, url: String },
    ListItem(Vec<AboutInline>),
    Hyperlink(String),
    Code(String),
    Text(Vec<AboutInline>),
}

fn parse_inline_markdown(text: &str) -> Vec<AboutInline> {
    let mut spans = Vec::new();
    let mut remaining = text;
    let mut strong = false;

    while let Some(index) = remaining.find("**") {
        let (before, after) = remaining.split_at(index);
        if !before.is_empty() {
            spans.push(AboutInline {
                text: before.to_string(),
                strong,
            });
        }
        strong = !strong;
        remaining = &after[2..];
    }

    if !remaining.is_empty() || spans.is_empty() {
        spans.push(AboutInline {
            text: remaining.to_string(),
            strong,
        });
    }

    spans
}

fn parse_about_markdown(markdown: &str) -> Vec<AboutElement> {
    let mut elements = Vec::new();
    let mut in_code = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();

        // Fenced code blocks: drop the ``` delimiters and render the inner
        // lines verbatim as monospace so embedded shell/TOML snippets stay
        // legible in the License/Licensing tabs.
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            elements.push(AboutElement::Code(line.to_string()));
            continue;
        }

        let element = if let Some(header) = trimmed.strip_prefix("# ") {
            AboutElement::Heading1(header.to_string())
        } else if let Some(header) = trimmed.strip_prefix("## ") {
            AboutElement::Heading2(header.to_string())
        } else if let Some(header) = trimmed.strip_prefix("### ") {
            AboutElement::Heading3(header.to_string())
        } else if let Some(item) = trimmed.strip_prefix("- ") {
            if let Some((label, url)) = item.rsplit_once(": ")
                && (url.starts_with("http://") || url.starts_with("https://"))
            {
                AboutElement::ListItemLink {
                    label: label.to_string(),
                    url: url.to_string(),
                }
            } else {
                AboutElement::ListItem(parse_inline_markdown(item))
            }
        } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            AboutElement::Hyperlink(trimmed.to_string())
        } else {
            AboutElement::Text(parse_inline_markdown(line))
        };
        elements.push(element);
    }

    elements
}

/// Lazily-parsed markdown for each text-based tab, parsed once on first view.
static PARSED_ABOUT: std::sync::LazyLock<Vec<AboutElement>> =
    std::sync::LazyLock::new(|| parse_about_markdown(EMBEDDED_ABOUT));
static PARSED_LICENSE: std::sync::LazyLock<Vec<AboutElement>> =
    std::sync::LazyLock::new(|| parse_about_markdown(EMBEDDED_LICENSE));
static PARSED_LICENSING: std::sync::LazyLock<Vec<AboutElement>> =
    std::sync::LazyLock::new(|| parse_about_markdown(EMBEDDED_LICENSING));
/// The third-party notices file is large (hundreds of KB), so it is rendered as
/// virtualized monospace rows rather than parsed markdown.
static THIRD_PARTY_LINES: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| EMBEDDED_THIRD_PARTY.lines().collect());
/// Widest line (in characters) in the third-party notices, used to pin a stable
/// horizontal content width so scrolling does not relayout every frame.
static THIRD_PARTY_MAX_CHARS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    THIRD_PARTY_LINES
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
});

fn markdown_job(
    text: &[AboutInline],
    font_size: f32,
    color: Color32,
    strong_color: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    for span in text {
        job.append(
            span.text.as_str(),
            0.0,
            TextFormat {
                font_id: FontId::proportional(font_size),
                color: if span.strong { strong_color } else { color },
                ..Default::default()
            },
        );
    }
    job
}

impl Foxy {
    pub fn render_about_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let fonts = self.settings_view_state.font_sizes.about_view.clone();
        let about_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        let about_frame = Frame::NONE.inner_margin(about_margin);
        about_frame.show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(self.t("About"));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close_button = ui.add_sized(
                            Vec2::new(30.0, 30.0),
                            Button::new(RichText::new("X").color(self.color_text_normal()))
                                .fill(self.color_main_bg()),
                        );
                        if close_button.hovered() {
                            ui.ctx()
                                .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                        }
                        if close_button.clicked() {
                            info!("Closing about view");
                            self.close_reference_view();
                        }
                    });
                });

                ui.separator();

                ui.label(
                    RichText::new(crate::build_info::version_label())
                        .size(fonts.body as f32)
                        .color(self.color_text_normal()),
                );

                ui.add_space(8.0);
                self.render_about_tabs(ui, &fonts);
                ui.separator();

                match self.current_about_tab {
                    AboutTab::About => {
                        self.render_about_markdown_tab(ui, "about_tab_about", &PARSED_ABOUT, &fonts)
                    }
                    AboutTab::License => self.render_about_markdown_tab(
                        ui,
                        "about_tab_license",
                        &PARSED_LICENSE,
                        &fonts,
                    ),
                    AboutTab::Licensing => self.render_about_markdown_tab(
                        ui,
                        "about_tab_licensing",
                        &PARSED_LICENSING,
                        &fonts,
                    ),
                    AboutTab::ThirdPartyLicenses => {
                        self.render_about_third_party(ui, fonts.body as f32)
                    }
                }
            });
        });
    }

    fn render_about_tabs(&mut self, ui: &mut Ui, fonts: &AboutViewFonts) {
        ui.horizontal_wrapped(|ui| {
            for tab in AboutTab::all_tabs() {
                let is_selected = self.current_about_tab == tab;
                let color = if is_selected {
                    self.color_primary_accent()
                } else {
                    self.color_main_bg()
                };

                let tab_button = ui.add(
                    Button::new(
                        RichText::new(self.t(tab.as_str()))
                            .color(self.color_text_normal())
                            .size(fonts.body as f32),
                    )
                    .fill(color),
                );

                if tab_button.hovered() {
                    ui.ctx()
                        .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                }

                if tab_button.clicked() {
                    self.current_about_tab = tab;
                    info!("Switched about tab to {}", tab.as_str());
                }

                ui.add_space(4.0);
            }
        });
    }

    fn render_about_markdown_tab(
        &self,
        ui: &mut Ui,
        id_salt: &str,
        elements: &[AboutElement],
        fonts: &AboutViewFonts,
    ) {
        ScrollArea::vertical()
            .id_salt(id_salt)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for element in elements {
                    match element {
                        AboutElement::Heading1(text) => {
                            ui.add_space(8.0);
                            ui.heading(
                                RichText::new(text.as_str())
                                    .size(fonts.h1 as f32)
                                    .strong()
                                    .color(self.color_text_normal()),
                            );
                        }
                        AboutElement::Heading2(text) => {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(text.as_str())
                                    .size(fonts.h2 as f32)
                                    .strong()
                                    .color(self.color_text_normal()),
                            );
                        }
                        AboutElement::Heading3(text) => {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(text.as_str())
                                    .size(fonts.h3 as f32)
                                    .strong()
                                    .color(self.color_text_normal()),
                            );
                        }
                        AboutElement::ListItemLink { label, url } => {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("- ")
                                        .size(fonts.body as f32)
                                        .color(self.color_text_normal()),
                                );
                                ui.hyperlink_to(
                                    RichText::new(label.as_str()).size(fonts.body as f32),
                                    url.as_str(),
                                );
                            });
                        }
                        AboutElement::ListItem(item) => {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("- ")
                                        .size(fonts.body as f32)
                                        .color(self.color_text_normal()),
                                );
                                ui.add(
                                    Label::new(markdown_job(
                                        item,
                                        fonts.body as f32,
                                        self.color_text_normal(),
                                        ui.visuals().strong_text_color(),
                                    ))
                                    .wrap(),
                                );
                            });
                        }
                        AboutElement::Hyperlink(url) => {
                            ui.add(Hyperlink::from_label_and_url(
                                RichText::new(url.as_str()).size(fonts.body as f32),
                                url.as_str(),
                            ));
                        }
                        AboutElement::Code(text) => {
                            ui.label(
                                RichText::new(text.as_str())
                                    .monospace()
                                    .size(fonts.body as f32)
                                    .color(self.color_text_normal()),
                            );
                        }
                        AboutElement::Text(line) => {
                            ui.add(
                                Label::new(markdown_job(
                                    line,
                                    fonts.body as f32,
                                    self.color_text_normal(),
                                    ui.visuals().strong_text_color(),
                                ))
                                .wrap(),
                            );
                        }
                    }
                }
            });
    }

    /// Render the (large) third-party notices file as virtualized monospace
    /// rows so only the visible lines are shaped each frame. Long lines extend
    /// horizontally rather than wrapping, with both scrollbars available.
    ///
    /// The horizontal content width is pinned to the widest line up front. If it
    /// were derived from the currently visible rows instead, it would change as
    /// you scroll, toggling the horizontal scrollbar and forcing egui into a
    /// per-frame relayout ("changed id between passes" multi-pass).
    fn render_about_third_party(&self, ui: &mut Ui, font_size: f32) {
        let font_id = FontId::monospace(font_size);
        let (row_height, glyph_width) =
            ui.fonts_mut(|f| (f.row_height(&font_id), f.glyph_width(&font_id, 'M')));
        let lines = THIRD_PARTY_LINES.as_slice();
        let content_width = (*THIRD_PARTY_MAX_CHARS as f32) * glyph_width + 16.0;
        let color = self.color_text_normal();

        ScrollArea::both()
            .id_salt("about_tab_third_party")
            .auto_shrink([false; 2])
            .show_rows(ui, row_height, lines.len(), |ui, row_range| {
                ui.set_width(content_width);
                for row in row_range {
                    let line = lines[row];
                    // Empty lines must still occupy a full row so the
                    // virtualized layout stays aligned with the scrollbar.
                    let text = if line.is_empty() { " " } else { line };
                    ui.add(
                        Label::new(RichText::new(text).monospace().size(font_size).color(color))
                            .wrap_mode(TextWrapMode::Extend),
                    );
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bold_inline_markdown() {
        let spans = parse_inline_markdown("A **bold** word");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "A ");
        assert!(!spans[0].strong);
        assert_eq!(spans[1].text, "bold");
        assert!(spans[1].strong);
        assert_eq!(spans[2].text, " word");
        assert!(!spans[2].strong);
    }

    #[test]
    fn parses_about_heading_levels() {
        let parsed = parse_about_markdown("# H1\n## H2\n### H3");
        assert!(matches!(parsed[0], AboutElement::Heading1(_)));
        assert!(
            parsed
                .iter()
                .any(|element| matches!(element, AboutElement::Heading2(text) if text == "H2"))
        );
        assert!(
            parsed
                .iter()
                .any(|element| matches!(element, AboutElement::Heading3(text) if text == "H3"))
        );
    }

    #[test]
    fn fenced_code_blocks_drop_delimiters_and_keep_content() {
        let parsed = parse_about_markdown("intro\n```text\ncargo build\n```\noutro");
        // The ``` fence lines are dropped; the inner line is a Code element.
        let code: Vec<&str> = parsed
            .iter()
            .filter_map(|element| match element {
                AboutElement::Code(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(code, vec!["cargo build"]);
        assert!(
            !parsed
                .iter()
                .any(|element| matches!(element, AboutElement::Text(spans)
                    if spans.iter().any(|span| span.text.contains("```"))))
        );
    }

    #[test]
    fn embedded_license_documents_are_non_empty() {
        assert!(!EMBEDDED_LICENSE.trim().is_empty());
        assert!(!EMBEDDED_LICENSING.trim().is_empty());
        assert!(!EMBEDDED_THIRD_PARTY.trim().is_empty());
        assert!(!PARSED_LICENSE.is_empty());
        assert!(!PARSED_LICENSING.is_empty());
        assert!(!THIRD_PARTY_LINES.is_empty());
    }
}
