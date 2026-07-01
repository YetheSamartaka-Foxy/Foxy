mod input;
mod style;
mod widgets;

use crate::ui::app::Foxy;

impl Foxy {
    pub fn t(&self, key: &str) -> String {
        self.i18n.tr(key)
    }

    pub fn t_fmt(&self, key: &str, replacements: &[(&str, String)]) -> String {
        self.i18n.tr_fmt(key, replacements)
    }
}
