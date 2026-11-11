use eframe::egui::{Event, Key, Modifiers, RawInput};

pub(super) fn key_events(key: Key, modifiers: Modifiers) -> [Event; 2] {
    [
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        },
        Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}

#[derive(Debug)]
pub(super) struct NavigationSteps {
    remaining: u32,
    modifiers: Modifiers,
}

impl NavigationSteps {
    pub(super) fn new(count: u32, reverse: bool) -> Self {
        Self {
            remaining: count.max(1),
            modifiers: Modifiers {
                shift: reverse,
                ..Default::default()
            },
        }
    }

    pub(super) fn apply_frame(&mut self, raw_input: &mut RawInput) -> bool {
        if self.is_finished() {
            return false;
        }

        raw_input
            .events
            .push(Event::ModifiersChanged(self.modifiers));
        raw_input
            .events
            .extend(key_events(Key::Tab, self.modifiers));
        self.remaining -= 1;
        true
    }

    pub(super) fn is_finished(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{self, Id, Pos2, Rect, Vec2, ViewportId};

    fn run_frame(ctx: &egui::Context, input: RawInput, render: impl FnMut(&mut egui::Ui)) {
        let mut output = ctx.run_ui(input, render);
        output.textures_delta.clear();
    }

    fn raw_frame() -> RawInput {
        let mut raw_input = RawInput {
            focused: false,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
            ..Default::default()
        };
        raw_input
            .viewports
            .get_mut(&ViewportId::ROOT)
            .unwrap()
            .focused = Some(false);
        raw_input
    }

    fn three_buttons(ctx: &egui::Context, raw_input: RawInput) -> ([Option<Id>; 3], [bool; 3]) {
        let mut ids = [None; 3];
        run_frame(ctx, raw_input, |ui| {
            for (index, label) in ["First", "Second", "Third"].into_iter().enumerate() {
                let response = ui.push_id(index, |ui| ui.button(label)).inner;
                ids[index] = Some(response.id);
            }
        });
        let focused = ctx.memory(|memory| memory.focused());
        (ids, std::array::from_fn(|index| ids[index] == focused))
    }

    #[test]
    fn key_helper_emits_a_press_and_release() {
        let events = key_events(Key::Enter, Modifiers::SHIFT);
        assert!(matches!(
            events[0],
            Event::Key {
                key: Key::Enter,
                pressed: true,
                modifiers: Modifiers::SHIFT,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            Event::Key {
                key: Key::Enter,
                pressed: false,
                modifiers: Modifiers::SHIFT,
                ..
            }
        ));
    }

    #[test]
    fn navigation_advances_once_per_frame_and_preserves_native_focus() {
        let ctx = egui::Context::default();
        let (ids, _) = three_buttons(&ctx, raw_frame());
        let mut forward = NavigationSteps::new(2, false);
        let mut focused = [false; 3];
        while !forward.is_finished() {
            let mut raw_input = raw_frame();
            assert!(forward.apply_frame(&mut raw_input));
            assert!(!raw_input.focused);
            assert_eq!(raw_input.viewports[&ViewportId::ROOT].focused, Some(false));
            assert_eq!(raw_input.events.len(), 3);
            focused = three_buttons(&ctx, raw_input).1;
        }
        assert!(!forward.apply_frame(&mut raw_frame()));
        assert_eq!(focused, [false, true, false]);
        assert_eq!(ctx.memory(|memory| memory.focused()), ids[1]);

        let mut reverse = NavigationSteps::new(1, true);
        let mut raw_input = raw_frame();
        assert!(reverse.apply_frame(&mut raw_input));
        three_buttons(&ctx, raw_input);
        let focused_after_reverse = three_buttons(&ctx, raw_frame()).1;
        assert_eq!(focused_after_reverse, [true, false, false]);

        let mut default_navigation = NavigationSteps::new(0, false);
        let mut raw_input = raw_frame();
        assert!(default_navigation.apply_frame(&mut raw_input));
        assert!(default_navigation.is_finished());
        assert_eq!(raw_input.events.len(), 3);
    }

    #[test]
    fn raw_key_text_and_pointer_events_are_derived_by_the_next_ui_pass() {
        let ctx = egui::Context::default();
        let mut activated = 0;
        let mut button_id = None;
        run_frame(&ctx, raw_frame(), |ui| {
            let response = ui.button("Activate");
            button_id = Some(response.id);
        });
        ctx.memory_mut(|memory| memory.request_focus(button_id.unwrap()));

        let mut key_input = RawInput {
            focused: true,
            ..raw_frame()
        };
        key_input
            .events
            .extend(key_events(Key::Enter, Modifiers::NONE));
        run_frame(&ctx, key_input, |ui| {
            let response = ui.button("Activate");
            if response.clicked() {
                activated += 1;
            }
        });
        assert_eq!(activated, 1);

        let mut text = String::new();
        let text_id = Id::unique("agent-driver-input-text");
        ctx.memory_mut(|memory| memory.request_focus(text_id));
        let mut text_input = RawInput {
            focused: true,
            ..raw_frame()
        };
        text_input.events.push(Event::Text("typed".to_owned()));
        run_frame(&ctx, text_input, |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(text_id));
        });
        assert_eq!(text, "typed");

        let mut clicks = 0;
        let mut click_rect = Rect::NOTHING;
        run_frame(&ctx, raw_frame(), |ui| {
            click_rect = ui
                .add_sized(Vec2::new(120.0, 32.0), egui::Button::new("Click"))
                .rect;
        });
        let click_position = click_rect.center();
        let mut pointer_input = raw_frame();
        pointer_input.events.extend([
            Event::PointerMoved(click_position),
            Event::PointerButton {
                pos: click_position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos: click_position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ]);
        run_frame(&ctx, pointer_input, |ui| {
            let response = ui.add_sized(Vec2::new(120.0, 32.0), egui::Button::new("Click"));
            clicks += usize::from(response.clicked());
        });
        assert_eq!(clicks, 1);
    }
}
