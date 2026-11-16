use super::{fields::Arma3FieldKind as K, optional_fields};
use serde_json::Number;

optional_fields!(Arma3DifficultyOptions {
    reduced_damage: bool => ("reducedDamage", K::Bool, "Reduced damage", 158),
    group_indicators: u8 => ("groupIndicators", K::Integer { min: 0, max: 2 }, "Group indicators: 0 never, 1 limited distance, 2 always", 158),
    friendly_tags: u8 => ("friendlyTags", K::Integer { min: 0, max: 2 }, "Friendly tags: 0 never, 1 limited distance, 2 always", 158),
    enemy_tags: u8 => ("enemyTags", K::Integer { min: 0, max: 2 }, "Enemy tags: 0 never, 1 limited distance, 2 always", 158),
    detected_mines: u8 => ("detectedMines", K::Integer { min: 0, max: 2 }, "Detected mines: 0 never, 1 limited distance, 2 always", 158),
    commands: u8 => ("commands", K::Integer { min: 0, max: 2 }, "Commands: 0 never, 1 fade out, 2 always", 158),
    waypoints: u8 => ("waypoints", K::Integer { min: 0, max: 2 }, "Waypoints: 0 never, 1 fade out, 2 always", 158),
    tactical_ping: u8 => ("tacticalPing", K::Integer { min: 0, max: 3 }, "Tactical ping: 0 disabled, 1 3D, 2 map, 3 3D and map", 158),
    weapon_info: u8 => ("weaponInfo", K::Integer { min: 0, max: 2 }, "Weapon info: 0 never, 1 fade out, 2 always", 158),
    stance_indicator: u8 => ("stanceIndicator", K::Integer { min: 0, max: 2 }, "Stance indicator: 0 never, 1 fade out, 2 always", 158),
    stamina_bar: bool => ("staminaBar", K::Bool, "Stamina bar", 158),
    weapon_crosshair: bool => ("weaponCrosshair", K::Bool, "Weapon crosshair", 158),
    vision_aid: bool => ("visionAid", K::Bool, "Vision aid", 158),
    third_person_view: u8 => ("thirdPersonView", K::Integer { min: 0, max: 2 }, "Third person view: 0 disabled, 1 enabled, 2 vehicles only", 158),
    camera_shake: bool => ("cameraShake", K::Bool, "Camera shake", 158),
    score_table: bool => ("scoreTable", K::Bool, "Score table", 158),
    death_messages: bool => ("deathMessages", K::Bool, "Killed by messages", 158),
    von_id: bool => ("vonID", K::Bool, "Voice speaker indicator", 158),
    map_content_friendly: bool => ("mapContentFriendly", K::Bool, "Friendly map content", 168),
    map_content_enemy: bool => ("mapContentEnemy", K::Bool, "Enemy map content", 168),
    map_content_mines: bool => ("mapContentMines", K::Bool, "Mine map content", 168),
    auto_report: bool => ("autoReport", K::Bool, "Automatic reporting", 158),
    multiple_saves: bool => ("multipleSaves", K::Bool, "Multiple saves", 158),
});

optional_fields!(Arma3CustomAi {
    skill_ai: Number => ("skillAI", K::Decimal { min: 0.0, max: 1.0 }, "Custom AI skill", 158),
    precision_ai: Number => ("precisionAI", K::Decimal { min: 0.0, max: 1.0 }, "Custom AI precision", 158),
});

pub const ARMA3_AI_PRESETS: &[(u8, &str)] =
    &[(0, "Low"), (1, "Normal"), (2, "High"), (3, "Custom")];

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3DifficultyProfile {
    #[serde(default, skip_serializing_if = "Arma3DifficultyOptions::is_empty")]
    pub options: Arma3DifficultyOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_level_preset: Option<u8>,
    #[serde(default, skip_serializing_if = "Arma3CustomAi::is_empty")]
    pub custom_ai: Arma3CustomAi,
}

impl Arma3DifficultyProfile {
    pub fn validate(&self) -> Result<(), String> {
        self.options.validate()?;
        self.custom_ai.validate()?;
        if self.ai_level_preset.is_some_and(|preset| preset > 3) {
            return Err("AI preset must be 0 Low, 1 Normal, 2 High or 3 Custom".into());
        }
        if !self.custom_ai.is_empty() && self.ai_level_preset != Some(3) {
            return Err("Custom AI skill and precision require AI preset 3 Custom".into());
        }
        Ok(())
    }

    pub fn validate_version(&self, version: u16) -> Result<(), String> {
        super::fields::validate_version(&self.options, Arma3DifficultyOptions::FIELDS, version)?;
        super::fields::validate_version(&self.custom_ai, Arma3CustomAi::FIELDS, version)
    }

    /// Renders the profile file the engine reads for `difficulty = "Custom"`. The engine writes
    /// `version=1;` in dedicated profiles it creates itself at 2.22.
    pub fn render(&self) -> Result<String, String> {
        self.validate()?;
        let options = self.options.render()?;
        let preset = self
            .ai_level_preset
            .map(|preset| format!("aiLevelPreset = {preset};\n"))
            .unwrap_or_default();
        let custom = if self.custom_ai.is_empty() {
            String::new()
        } else {
            format!("class CustomAILevel\n{{\n{}}};\n", self.custom_ai.render()?)
        };
        Ok(format!(
            "version = 1;\nclass DifficultyPresets\n{{\nclass CustomDifficulty\n{{\nclass Options\n{{\n{options}}};\n{preset}}};\n{custom}}};\n"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_ai_requires_the_custom_preset_and_domains_are_field_specific() {
        let mut profile = Arma3DifficultyProfile::default();
        profile.options.third_person_view = Some(2);
        profile.options.tactical_ping = Some(3);
        assert!(profile.validate().is_ok());
        profile.custom_ai.precision_ai = Number::from_f64(0.25);
        assert!(profile.validate().is_err());
        profile.ai_level_preset = Some(3);
        assert!(profile.validate().is_ok());
        profile.options.friendly_tags = Some(3);
        assert!(profile.validate().is_err());
        profile.options.friendly_tags = Some(2);
        profile.options.tactical_ping = Some(4);
        assert!(profile.validate().is_err());
        profile.options.tactical_ping = None;
        profile.custom_ai.skill_ai = Number::from_f64(1.5);
        assert!(profile.validate().is_err());
        profile.ai_level_preset = Some(4);
        profile.custom_ai = Arma3CustomAi::default();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn version_gates_cover_the_overhaul_and_split_map_content() {
        let mut profile = Arma3DifficultyProfile::default();
        profile.options.weapon_crosshair = Some(false);
        assert!(profile.validate_version(156).is_err());
        assert!(profile.validate_version(158).is_ok());
        profile.options.map_content_mines = Some(true);
        assert!(profile.validate_version(166).is_err());
        assert!(profile.validate_version(168).is_ok());
    }

    #[test]
    fn empty_profile_keeps_class_nesting_and_custom_ai_is_a_sibling() {
        assert_eq!(
            Arma3DifficultyProfile::default().render().unwrap(),
            "version = 1;\nclass DifficultyPresets\n{\nclass CustomDifficulty\n{\nclass Options\n{\n};\n};\n};\n"
        );
        let profile = Arma3DifficultyProfile {
            ai_level_preset: Some(3),
            custom_ai: Arma3CustomAi {
                skill_ai: Number::from_f64(0.35),
                precision_ai: Number::from_f64(0.25),
            },
            ..Default::default()
        };
        assert_eq!(
            profile.render().unwrap(),
            "version = 1;\nclass DifficultyPresets\n{\nclass CustomDifficulty\n{\nclass Options\n{\n};\naiLevelPreset = 3;\n};\nclass CustomAILevel\n{\nskillAI = 0.35;\nprecisionAI = 0.25;\n};\n};\n"
        );
    }
}
