//! Omitted managed configuration fields are not rendered, preserving the engine's own defaults.

mod basic;
mod difficulty;
mod fields;
mod server;

pub use basic::Arma3BasicSettings;
pub use difficulty::{
    ARMA3_AI_PRESETS, Arma3CustomAi, Arma3DifficultyOptions, Arma3DifficultyProfile,
};
pub use fields::{
    ARMA3_CHANNELS, ARMA3_KICK_CAUSES, Arma3Field, Arma3FieldKind, Arma3ListKind,
    parse_decimal_input,
};
pub(crate) use fields::{number_text, quoted, render_section, validate_section, validate_version};
#[cfg(test)]
pub(crate) use server::Arma3AntiFlood;
pub(crate) use server::profile_name_valid;
pub use server::{
    ARMA3_DEFAULT_BASIC_CONFIG, ARMA3_DEFAULT_PROFILE, ARMA3_DEFAULT_SERVER_CONFIG,
    Arma3ConfigurationNames, Arma3ServerSettings,
};

macro_rules! optional_fields {
    ($name:ident { $($field:ident : $ty:ty => ($engine:literal, $kind:expr, $label:literal, $since:literal)),* $(,)? }) => {
        #[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            $(#[serde(default, skip_serializing_if = "Option::is_none")]
            pub $field: Option<$ty>,)*
        }

        #[allow(dead_code, reason = "every generated section exposes the same API")]
        impl $name {
            pub const FIELDS: &'static [super::fields::Arma3Field] = &[
                $(super::fields::Arma3Field {
                    json_name: stringify!($field),
                    engine_name: $engine,
                    kind: $kind,
                    label: $label,
                    since: $since,
                },)*
            ];

            pub fn is_empty(&self) -> bool {
                true $(&& self.$field.is_none())*
            }

            pub fn validate(&self) -> Result<(), String> {
                super::validate_section(self, Self::FIELDS)
            }

            pub fn render(&self) -> Result<String, String> {
                super::render_section(self, Self::FIELDS)
            }
        }
    };
}
pub(crate) use optional_fields;

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(fields: &[Arma3Field], out: &mut Vec<&'static str>) {
        for field in fields {
            out.push(field.label);
            if let Arma3FieldKind::Section(nested) = field.kind {
                labels(nested, out);
            }
            if let Arma3FieldKind::Choice(choices) = field.kind {
                out.extend(choices.iter().map(|(_, label)| *label));
            }
        }
    }

    #[test]
    fn every_form_label_has_an_english_locale_entry() {
        let english: serde_json::Value =
            serde_json::from_str(include_str!("../../../ui/locales/en.json")).unwrap();
        let mut all = Vec::new();
        for fields in [
            Arma3ServerSettings::FIELDS,
            Arma3BasicSettings::FIELDS,
            Arma3DifficultyOptions::FIELDS,
            Arma3CustomAi::FIELDS,
        ] {
            labels(fields, &mut all);
        }
        all.extend(ARMA3_KICK_CAUSES.iter().map(|(_, label)| *label));
        all.extend(ARMA3_CHANNELS.iter().map(|(_, label)| *label));
        all.extend(ARMA3_AI_PRESETS.iter().map(|(_, label)| *label));
        let missing = all
            .into_iter()
            .filter(|label| english.get(*label).is_none())
            .collect::<Vec<_>>();
        assert!(missing.is_empty(), "missing English keys: {missing:#?}");
    }

    #[test]
    fn engine_names_are_unique_within_each_section() {
        for fields in [
            Arma3ServerSettings::FIELDS,
            Arma3BasicSettings::FIELDS,
            Arma3DifficultyOptions::FIELDS,
            Arma3CustomAi::FIELDS,
            Arma3AntiFlood::FIELDS,
            server::Arma3AdvancedOptions::FIELDS,
        ] {
            let mut names = fields
                .iter()
                .map(|field| field.engine_name.to_ascii_lowercase())
                .collect::<Vec<_>>();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(before, names.len());
        }
    }
}
