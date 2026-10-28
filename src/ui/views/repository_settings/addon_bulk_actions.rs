use crate::ui::i18n::tr;
use eframe::egui::{CursorIcon, Response, Ui};

pub(super) fn render_filtered_only_checkbox(ui: &mut Ui, filtered_only: &mut bool) -> Response {
    ui.push_id("addon_bulk_filtered_only", |ui| {
        ui.checkbox(filtered_only, tr("Filtered only"))
            .on_hover_text(tr("When checked, Enable all and Disable all affect only addons matching the current filters. When unchecked, they affect all addons in this tab."))
            .on_hover_cursor(CursorIcon::PointingHand)
    }).inner
}

pub(super) fn set_bulk_addon_enabled(
    enabled_states: &mut [bool],
    filtered_indices: Option<&[usize]>,
    enabled: bool,
) -> bool {
    let mut changed = false;
    let mut set_enabled = |current: &mut bool| {
        changed |= *current != enabled;
        *current = enabled;
    };
    if let Some(indices) = filtered_indices {
        for &index in indices {
            if let Some(current) = enabled_states.get_mut(index) {
                set_enabled(current);
            }
        }
    } else {
        for current in enabled_states {
            set_enabled(current);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{render_filtered_only_checkbox, set_bulk_addon_enabled};
    use eframe::egui::{self, Event, Key, Modifiers, Response};

    fn keyboard_frame(
        ctx: &egui::Context,
        filtered_only: &mut bool,
        key: Option<Key>,
        shift: bool,
    ) -> [Response; 3] {
        let modifiers = Modifiers {
            shift,
            ..Default::default()
        };
        let mut events = key
            .map(|key| {
                [true, false]
                    .map(|pressed| Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    })
                    .to_vec()
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let mut responses = None;
        let mut render = |ui: &mut egui::Ui| {
            responses = Some([
                ui.button("Before"),
                render_filtered_only_checkbox(ui, filtered_only),
                ui.button("After"),
            ]);
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            &mut render,
        );
        output.textures_delta.clear();
        ctx.run_ui(egui::RawInput::default(), render)
            .textures_delta
            .clear();
        responses.expect("checkbox rendered")
    }

    #[test]
    fn filtered_only_checkbox_supports_keyboard_navigation_and_activation() {
        let ctx = egui::Context::default();
        let mut filtered_only = false;
        let responses = keyboard_frame(&ctx, &mut filtered_only, None, false);
        assert!(!filtered_only);
        ctx.memory_mut(|memory| memory.request_focus(responses[0].id));

        let responses = keyboard_frame(&ctx, &mut filtered_only, Some(Key::Tab), false);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(responses[1].id));
        keyboard_frame(&ctx, &mut filtered_only, Some(Key::Enter), false);
        assert!(filtered_only);

        let responses = keyboard_frame(&ctx, &mut filtered_only, Some(Key::Tab), false);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(responses[2].id));
        let responses = keyboard_frame(&ctx, &mut filtered_only, Some(Key::Tab), true);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(responses[1].id));
        keyboard_frame(&ctx, &mut filtered_only, Some(Key::Enter), false);
        assert!(!filtered_only);

        let responses = keyboard_frame(&ctx, &mut filtered_only, Some(Key::ArrowDown), false);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(responses[2].id));
        let responses = keyboard_frame(&ctx, &mut filtered_only, Some(Key::ArrowUp), false);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(responses[1].id));
    }

    #[test]
    fn bulk_addon_actions_default_to_all_rows() {
        let mut states = [false, true, false];
        assert!(set_bulk_addon_enabled(&mut states, None, true));
        assert_eq!(states, [true; 3]);
        assert!(!set_bulk_addon_enabled(&mut states, None, true));
        assert!(set_bulk_addon_enabled(&mut states, None, false));
        assert_eq!(states, [false; 3]);
    }

    #[test]
    fn bulk_addon_actions_only_change_filtered_rows() {
        let mut states = [true, false, true, false];
        let indices = [3, 1];
        assert!(set_bulk_addon_enabled(&mut states, Some(&indices), true));
        assert_eq!(states, [true; 4]);
        assert!(set_bulk_addon_enabled(&mut states, Some(&indices), false));
        assert_eq!(states, [true, false, true, false]);
        assert!(!set_bulk_addon_enabled(&mut states, Some(&indices), false));
    }

    #[test]
    fn bulk_addon_actions_with_no_matches_leave_all_rows_unchanged() {
        let mut states = [true, false];
        assert!(!set_bulk_addon_enabled(&mut states, Some(&[]), true));
        assert_eq!(states, [true, false]);
        assert!(!set_bulk_addon_enabled(&mut [], None, false));
    }

    #[test]
    fn bulk_addon_actions_include_matches_outside_the_viewport() {
        let mut states = vec![false; 500];
        assert!(set_bulk_addon_enabled(
            &mut states,
            Some(&[0, 250, 499]),
            true
        ));
        assert_eq!(states.iter().filter(|enabled| **enabled).count(), 3);
        assert!(states[0] && states[250] && states[499]);
    }
}
