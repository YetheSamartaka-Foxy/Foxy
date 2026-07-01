use crate::ui::app::Foxy;
use eframe::egui::{
    Align, Button, CursorIcon, Frame, Label, Layout, Margin, RichText, ScrollArea, Ui, Vec2,
};
use log::info;

const EMBEDDED_CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// Pre-parsed element from the embedded changelog markdown.
#[derive(Clone)]
enum ChangelogElement {
    Heading1(String),
    Heading2(String),
    Heading3(String),
    ListItem(String),
    Text(String),
}

fn parse_changelog() -> Vec<ChangelogElement> {
    parse_changelog_markdown(EMBEDDED_CHANGELOG)
}

fn parse_changelog_markdown(markdown: &str) -> Vec<ChangelogElement> {
    markdown
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if let Some(header) = trimmed.strip_prefix("# ") {
                ChangelogElement::Heading1(header.to_string())
            } else if let Some(header) = trimmed.strip_prefix("## ") {
                ChangelogElement::Heading2(header.to_string())
            } else if let Some(header) = trimmed.strip_prefix("### ") {
                ChangelogElement::Heading3(header.to_string())
            } else if let Some(item) = trimmed.strip_prefix("- ") {
                ChangelogElement::ListItem(format!("- {}", item))
            } else {
                ChangelogElement::Text(line.to_string())
            }
        })
        .collect()
}

fn changelog_versions(elements: &[ChangelogElement]) -> Vec<&str> {
    elements
        .iter()
        .filter_map(|element| match element {
            ChangelogElement::Heading1(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Lazily-initialized parsed changelog elements.
static PARSED_CHANGELOG: std::sync::LazyLock<Vec<ChangelogElement>> =
    std::sync::LazyLock::new(parse_changelog);

impl Foxy {
    pub fn render_changelog_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let fonts = self.settings_view_state.font_sizes.about_view.clone();
        let changelog_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        let changelog_frame = Frame::NONE.inner_margin(changelog_margin);
        changelog_frame.show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(self.t("Changelog"));

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
                            info!("Closing changelog view");
                            self.close_reference_view();
                        }
                    });
                });

                ui.separator();

                let mut scroll_target = None;
                let versions = changelog_versions(&PARSED_CHANGELOG);
                if !versions.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for version in versions {
                            let response = ui.link(version);
                            if response.hovered() {
                                ui.ctx()
                                    .output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
                            }
                            if response.clicked() {
                                scroll_target = Some(version.to_string());
                            }
                        }
                    });

                    ui.separator();
                }

                ScrollArea::vertical()
                    .id_salt("changelog_content")
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        for element in PARSED_CHANGELOG.iter() {
                            match element {
                                ChangelogElement::Heading1(text) => {
                                    ui.add_space(8.0);
                                    let response = ui.heading(
                                        RichText::new(text.as_str())
                                            .size(fonts.h1 as f32)
                                            .strong()
                                            .color(self.color_text_normal()),
                                    );
                                    if scroll_target.as_deref() == Some(text.as_str()) {
                                        response.scroll_to_me(Some(Align::Min));
                                    }
                                }
                                ChangelogElement::Heading2(text) => {
                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new(text.as_str())
                                            .size(fonts.h2 as f32)
                                            .strong()
                                            .color(self.color_text_normal()),
                                    );
                                }
                                ChangelogElement::Heading3(text) => {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(text.as_str())
                                            .size(fonts.h3 as f32)
                                            .strong()
                                            .color(self.color_text_normal()),
                                    );
                                }
                                ChangelogElement::ListItem(item) => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(item.as_str())
                                                .size(fonts.body as f32)
                                                .color(self.color_text_normal()),
                                        )
                                        .wrap(),
                                    );
                                }
                                ChangelogElement::Text(line) => {
                                    ui.add(
                                        Label::new(
                                            RichText::new(line.as_str())
                                                .size(fonts.body as f32)
                                                .color(self.color_text_normal()),
                                        )
                                        .wrap(),
                                    );
                                }
                            }
                        }
                    });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_changelog_heading_levels() {
        let parsed = parse_changelog_markdown("# H1\n## H2\n### H3");
        assert!(matches!(parsed[0], ChangelogElement::Heading1(_)));
        assert!(
            parsed
                .iter()
                .any(|element| matches!(element, ChangelogElement::Heading2(text) if text == "H2"))
        );
        assert!(
            parsed
                .iter()
                .any(|element| matches!(element, ChangelogElement::Heading3(text) if text == "H3"))
        );
    }

    #[test]
    fn extracts_version_links_from_h1_headings() {
        let parsed = parse_changelog_markdown("# 1.2.0\n## Added\n# 1.1.0\n- Fixed");

        assert_eq!(changelog_versions(&parsed), vec!["1.2.0", "1.1.0"]);
    }
}
