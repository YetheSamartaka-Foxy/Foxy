use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct FontSizeRange {
    pub min: u16,
    pub max: u16,
}

impl FontSizeRange {
    pub const fn new(min: u16, max: u16) -> Self {
        Self { min, max }
    }

    pub fn clamp(self, value: u16) -> u16 {
        value.clamp(self.min, self.max)
    }
}

pub const MAIN_WINDOW_CONTROL_ICONS_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const MAIN_ACTIVITY_LOG_TOGGLE_ICON_RANGE: FontSizeRange = FontSizeRange::new(12, 18);
pub const SETTINGS_PAGE_TITLE_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const SETTINGS_CLOSE_ICON_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const HELP_PAGE_TITLE_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const HELP_TAB_LABEL_RANGE: FontSizeRange = FontSizeRange::new(12, 20);
pub const HELP_SECTION_TITLE_RANGE: FontSizeRange = FontSizeRange::new(18, 26);
pub const HELP_BODY_RANGE: FontSizeRange = FontSizeRange::new(13, 22);
pub const ABOUT_H1_RANGE: FontSizeRange = FontSizeRange::new(24, 34);
pub const ABOUT_H2_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const ABOUT_H3_RANGE: FontSizeRange = FontSizeRange::new(18, 26);
pub const ABOUT_BODY_RANGE: FontSizeRange = FontSizeRange::new(14, 22);
pub const REPOSITORY_ADD_BUTTON_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const REPOSITORY_TOOLBAR_ICONS_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const REPOSITORY_STATUS_BANNER_RANGE: FontSizeRange = FontSizeRange::new(18, 28);
pub const REPOSITORY_LAUNCH_JOIN_BUTTONS_RANGE: FontSizeRange = FontSizeRange::new(18, 28);
pub const UPDATE_PAGE_TITLE_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const UPDATE_CLOSE_ICON_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const UPDATE_SECTION_TITLE_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const UPDATE_TOTAL_SIZE_RANGE: FontSizeRange = FontSizeRange::new(16, 24);
pub const UPDATE_SUMMARY_HEADING_RANGE: FontSizeRange = FontSizeRange::new(18, 26);
pub const UPDATE_SUMMARY_BODY_RANGE: FontSizeRange = FontSizeRange::new(12, 18);
pub const UPDATE_MOD_NAME_RANGE: FontSizeRange = FontSizeRange::new(16, 24);
pub const UPDATE_MOD_STATUS_RANGE: FontSizeRange = FontSizeRange::new(12, 18);
pub const UPDATE_MOD_PROGRESS_RANGE: FontSizeRange = FontSizeRange::new(12, 16);
pub const UPDATE_FILE_DETAILS_RANGE: FontSizeRange = FontSizeRange::new(12, 16);
pub const UPDATE_PAUSE_BUTTON_RANGE: FontSizeRange = FontSizeRange::new(14, 20);
pub const REPOSITORY_SETTINGS_PAGE_TITLE_RANGE: FontSizeRange = FontSizeRange::new(20, 30);
pub const REPOSITORY_SETTINGS_CLOSE_ICON_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const REPOSITORY_SETTINGS_REFRESH_ICON_RANGE: FontSizeRange = FontSizeRange::new(16, 22);
pub const REPOSITORY_SETTINGS_ADDON_PATH_RANGE: FontSizeRange = FontSizeRange::new(12, 18);

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Default)]
pub struct FontSizes {
    #[serde(default)]
    pub main_view: MainViewFonts,
    #[serde(default)]
    pub settings_view: SettingsViewFonts,
    #[serde(default)]
    pub help_view: HelpViewFonts,
    #[serde(default)]
    pub about_view: AboutViewFonts,
    #[serde(default)]
    pub repository_view: RepositoryViewFonts,
    #[serde(default)]
    pub update_view: UpdateViewFonts,
    #[serde(default)]
    pub repository_settings_view: RepositorySettingsViewFonts,
}

impl FontSizes {
    pub fn clamp_to_limits(&mut self) {
        self.main_view.clamp_to_limits();
        self.settings_view.clamp_to_limits();
        self.help_view.clamp_to_limits();
        self.about_view.clamp_to_limits();
        self.repository_view.clamp_to_limits();
        self.update_view.clamp_to_limits();
        self.repository_settings_view.clamp_to_limits();
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct MainViewFonts {
    pub window_control_icons: u16,
    pub activity_log_toggle_icon: u16,
}

impl Default for MainViewFonts {
    fn default() -> Self {
        Self {
            window_control_icons: 20,
            activity_log_toggle_icon: 14,
        }
    }
}

impl MainViewFonts {
    fn clamp_to_limits(&mut self) {
        self.window_control_icons =
            MAIN_WINDOW_CONTROL_ICONS_RANGE.clamp(self.window_control_icons);
        self.activity_log_toggle_icon =
            MAIN_ACTIVITY_LOG_TOGGLE_ICON_RANGE.clamp(self.activity_log_toggle_icon);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct SettingsViewFonts {
    pub page_title: u16,
    pub close_icon: u16,
}

impl Default for SettingsViewFonts {
    fn default() -> Self {
        Self {
            page_title: 24,
            close_icon: 20,
        }
    }
}

impl SettingsViewFonts {
    fn clamp_to_limits(&mut self) {
        self.page_title = SETTINGS_PAGE_TITLE_RANGE.clamp(self.page_title);
        self.close_icon = SETTINGS_CLOSE_ICON_RANGE.clamp(self.close_icon);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct HelpViewFonts {
    pub page_title: u16,
    pub tab_label: u16,
    pub section_title: u16,
    pub body: u16,
}

impl Default for HelpViewFonts {
    fn default() -> Self {
        Self {
            page_title: 24,
            tab_label: 14,
            section_title: 22,
            body: 15,
        }
    }
}

impl HelpViewFonts {
    fn clamp_to_limits(&mut self) {
        self.page_title = HELP_PAGE_TITLE_RANGE.clamp(self.page_title);
        self.tab_label = HELP_TAB_LABEL_RANGE.clamp(self.tab_label);
        self.section_title = HELP_SECTION_TITLE_RANGE.clamp(self.section_title);
        self.body = HELP_BODY_RANGE.clamp(self.body);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct AboutViewFonts {
    pub h1: u16,
    pub h2: u16,
    pub h3: u16,
    pub body: u16,
}

impl Default for AboutViewFonts {
    fn default() -> Self {
        Self {
            h1: 30,
            h2: 24,
            h3: 20,
            body: 16,
        }
    }
}

impl AboutViewFonts {
    fn clamp_to_limits(&mut self) {
        self.h1 = ABOUT_H1_RANGE.clamp(self.h1);
        self.h2 = ABOUT_H2_RANGE.clamp(self.h2);
        self.h3 = ABOUT_H3_RANGE.clamp(self.h3);
        self.body = ABOUT_BODY_RANGE.clamp(self.body);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositoryViewFonts {
    pub add_repository_button: u16,
    pub toolbar_icons: u16,
    pub status_banner: u16,
    pub launch_join_buttons: u16,
}

impl Default for RepositoryViewFonts {
    fn default() -> Self {
        Self {
            add_repository_button: 25,
            toolbar_icons: 20,
            status_banner: 22,
            launch_join_buttons: 25,
        }
    }
}

impl RepositoryViewFonts {
    fn clamp_to_limits(&mut self) {
        self.add_repository_button = REPOSITORY_ADD_BUTTON_RANGE.clamp(self.add_repository_button);
        self.toolbar_icons = REPOSITORY_TOOLBAR_ICONS_RANGE.clamp(self.toolbar_icons);
        self.status_banner = REPOSITORY_STATUS_BANNER_RANGE.clamp(self.status_banner);
        self.launch_join_buttons =
            REPOSITORY_LAUNCH_JOIN_BUTTONS_RANGE.clamp(self.launch_join_buttons);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct UpdateViewFonts {
    pub page_title: u16,
    pub close_icon: u16,
    pub section_title: u16,
    pub total_size: u16,
    pub summary_heading: u16,
    pub summary_body_fallback: u16,
    pub mod_name: u16,
    pub mod_status: u16,
    pub mod_progress: u16,
    pub file_details: u16,
    pub pause_button: u16,
}

impl Default for UpdateViewFonts {
    fn default() -> Self {
        Self {
            page_title: 30,
            close_icon: 20,
            section_title: 23,
            total_size: 16,
            summary_heading: 23,
            summary_body_fallback: 16,
            mod_name: 18,
            mod_status: 15,
            mod_progress: 13,
            file_details: 14,
            pause_button: 16,
        }
    }
}

impl UpdateViewFonts {
    /// One-time migration of the update-view heading hierarchy.
    ///
    /// Earlier builds shipped a hierarchy where the section title (26) was
    /// actually larger than the page title (24). For any field still left at
    /// its old default - i.e. the user never customized it - adopt the new
    /// default so the corrected hierarchy applies. Fields the user explicitly
    /// changed to some other value are left untouched.
    pub fn migrate_heading_hierarchy(&mut self) {
        const OLD_PAGE_TITLE: u16 = 24;
        const OLD_SECTION_TITLE: u16 = 26;
        const OLD_TOTAL_SIZE: u16 = 18;
        const OLD_SUMMARY_HEADING: u16 = 22;

        let defaults = Self::default();
        if self.page_title == OLD_PAGE_TITLE {
            self.page_title = defaults.page_title;
        }
        if self.section_title == OLD_SECTION_TITLE {
            self.section_title = defaults.section_title;
        }
        if self.total_size == OLD_TOTAL_SIZE {
            self.total_size = defaults.total_size;
        }
        if self.summary_heading == OLD_SUMMARY_HEADING {
            self.summary_heading = defaults.summary_heading;
        }
    }

    fn clamp_to_limits(&mut self) {
        self.page_title = UPDATE_PAGE_TITLE_RANGE.clamp(self.page_title);
        self.close_icon = UPDATE_CLOSE_ICON_RANGE.clamp(self.close_icon);
        self.section_title = UPDATE_SECTION_TITLE_RANGE.clamp(self.section_title);
        self.total_size = UPDATE_TOTAL_SIZE_RANGE.clamp(self.total_size);
        self.summary_heading = UPDATE_SUMMARY_HEADING_RANGE.clamp(self.summary_heading);
        self.summary_body_fallback = UPDATE_SUMMARY_BODY_RANGE.clamp(self.summary_body_fallback);
        self.mod_name = UPDATE_MOD_NAME_RANGE.clamp(self.mod_name);
        self.mod_status = UPDATE_MOD_STATUS_RANGE.clamp(self.mod_status);
        self.mod_progress = UPDATE_MOD_PROGRESS_RANGE.clamp(self.mod_progress);
        self.file_details = UPDATE_FILE_DETAILS_RANGE.clamp(self.file_details);
        self.pause_button = UPDATE_PAUSE_BUTTON_RANGE.clamp(self.pause_button);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositorySettingsViewFonts {
    pub page_title: u16,
    pub close_icon: u16,
    pub refresh_icon: u16,
    pub addon_path: u16,
}

impl Default for RepositorySettingsViewFonts {
    fn default() -> Self {
        Self {
            page_title: 24,
            close_icon: 20,
            refresh_icon: 20,
            addon_path: 14,
        }
    }
}

impl RepositorySettingsViewFonts {
    fn clamp_to_limits(&mut self) {
        self.page_title = REPOSITORY_SETTINGS_PAGE_TITLE_RANGE.clamp(self.page_title);
        self.close_icon = REPOSITORY_SETTINGS_CLOSE_ICON_RANGE.clamp(self.close_icon);
        self.refresh_icon = REPOSITORY_SETTINGS_REFRESH_ICON_RANGE.clamp(self.refresh_icon);
        self.addon_path = REPOSITORY_SETTINGS_ADDON_PATH_RANGE.clamp(self.addon_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_heading_hierarchy_updates_untouched_old_defaults() {
        // Every heading still sitting at its old default should adopt the new one.
        let mut fonts = UpdateViewFonts {
            page_title: 24,
            section_title: 26,
            total_size: 18,
            summary_heading: 22,
            ..UpdateViewFonts::default()
        };
        fonts.migrate_heading_hierarchy();

        let defaults = UpdateViewFonts::default();
        assert_eq!(fonts.page_title, defaults.page_title);
        assert_eq!(fonts.section_title, defaults.section_title);
        assert_eq!(fonts.total_size, defaults.total_size);
        assert_eq!(fonts.summary_heading, defaults.summary_heading);
    }

    #[test]
    fn migrate_heading_hierarchy_preserves_customized_values() {
        // Values the user explicitly changed (anything != the old default)
        // must be left untouched.
        let mut fonts = UpdateViewFonts {
            page_title: 28,
            section_title: 20,
            total_size: 22,
            summary_heading: 25,
            ..UpdateViewFonts::default()
        };
        fonts.migrate_heading_hierarchy();

        assert_eq!(fonts.page_title, 28);
        assert_eq!(fonts.section_title, 20);
        assert_eq!(fonts.total_size, 22);
        assert_eq!(fonts.summary_heading, 25);
    }

    #[test]
    fn migrate_heading_hierarchy_is_idempotent_on_new_defaults() {
        // Running against the current defaults must change nothing.
        let mut fonts = UpdateViewFonts::default();
        fonts.migrate_heading_hierarchy();
        assert_eq!(fonts, UpdateViewFonts::default());
    }
}
