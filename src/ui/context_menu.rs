use super::palette;
use eframe::egui::{Button, Response, RichText};

#[derive(Clone)]
pub struct ContextMenuItem<T: Copy> {
    pub id: T,
    pub label: String,
    pub enabled: bool,
    pub danger: bool,
    pub separator_before: bool,
}

impl<T: Copy> ContextMenuItem<T> {
    pub fn new(id: T, label: String) -> Self {
        Self {
            id,
            label,
            enabled: true,
            danger: false,
            separator_before: false,
        }
    }

    pub fn disabled_if(mut self, disabled: bool) -> Self {
        if disabled {
            self.enabled = false;
        }
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn separator_before(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

pub fn attach_context_menu<T: Copy>(
    response: &Response,
    items: &[ContextMenuItem<T>],
    selected_action: &mut Option<T>,
) {
    let mut picked = None;
    response.context_menu(|ui| {
        for item in items {
            if item.separator_before {
                ui.separator();
            }
            let mut button = Button::new(if item.danger {
                RichText::new(&item.label).color(palette::ERROR)
            } else {
                RichText::new(&item.label)
            });
            button = button.frame(false);
            let action_button = ui.add_enabled(item.enabled, button);
            if action_button.hovered() && item.enabled {
                ui.ctx()
                    .output_mut(|o| o.cursor_icon = eframe::egui::CursorIcon::PointingHand);
            }
            if action_button.clicked() && item.enabled {
                picked = Some(item.id);
                ui.close();
            }
        }
    });
    if picked.is_some() {
        *selected_action = picked;
    }
}
