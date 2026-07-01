/// State-filter keywords recognized by the repository filters. These are
/// matched against per-item state tags (see [`MultiEntryFilter::matches_with_tags`])
/// in addition to the usual name/address substring matching. Kept lowercase so
/// they can be compared directly against parsed (lowercased) filter entries.
pub const STATE_KEYWORD_INSTALLED: &str = "installed";
pub const STATE_KEYWORD_NOT_INSTALLED: &str = "not installed";
pub const STATE_KEYWORD_ATTACHED: &str = "attached";
pub const STATE_KEYWORD_DETACHED: &str = "detached";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultiEntryFilter {
    entries_lower: Vec<String>,
}

impl MultiEntryFilter {
    pub fn parse(input: &str) -> Self {
        Self {
            entries_lower: split_filter_entries(input)
                .into_iter()
                .map(|entry| entry.to_lowercase())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries_lower.is_empty()
    }

    pub fn matches_any_normalized(&self, values_lower: &[&str]) -> bool {
        self.is_empty()
            || self.entries_lower.iter().any(|entry| {
                values_lower
                    .iter()
                    .any(|value_lower| value_lower.contains(entry))
            })
    }

    pub fn matches_any(&self, values: &[&str]) -> bool {
        self.is_empty()
            || self.entries_lower.iter().any(|entry| {
                values
                    .iter()
                    .any(|value| value.to_lowercase().contains(entry))
            })
    }

    /// Like [`matches_any`](Self::matches_any), but an entry also matches when it
    /// is exactly equal to one of the provided `state_tags` (for example
    /// `"installed"` or `"attached"`). Tags must already be lowercase. This lets
    /// the repository filters recognize state keywords alongside the usual
    /// name/address substring matching.
    pub fn matches_with_tags(&self, values: &[&str], state_tags: &[&str]) -> bool {
        self.matches_with_tag_constraints(
            |entry| {
                values
                    .iter()
                    .any(|value| value.to_lowercase().contains(entry))
            },
            state_tags,
        )
    }

    /// Normalized (pre-lowercased `values_lower`) variant of
    /// [`matches_with_tags`](Self::matches_with_tags).
    pub fn matches_normalized_with_tags(&self, values_lower: &[&str], state_tags: &[&str]) -> bool {
        self.matches_with_tag_constraints(
            |entry| values_lower.iter().any(|value| value.contains(entry)),
            state_tags,
        )
    }

    fn matches_with_tag_constraints(
        &self,
        mut text_matches: impl FnMut(&str) -> bool,
        state_tags: &[&str],
    ) -> bool {
        if self.is_empty() {
            return true;
        }

        let mut has_text_entry = false;
        let mut text_entry_matches = false;
        let mut has_install_state_entry = false;
        let mut install_state_matches = false;
        let mut has_attach_state_entry = false;
        let mut attach_state_matches = false;

        for entry in &self.entries_lower {
            match entry.as_str() {
                STATE_KEYWORD_INSTALLED | STATE_KEYWORD_NOT_INSTALLED => {
                    has_install_state_entry = true;
                    install_state_matches |= state_tags.contains(&entry.as_str());
                }
                STATE_KEYWORD_ATTACHED | STATE_KEYWORD_DETACHED => {
                    has_attach_state_entry = true;
                    attach_state_matches |= state_tags.contains(&entry.as_str());
                }
                _ => {
                    has_text_entry = true;
                    text_entry_matches |= text_matches(entry);
                }
            }
        }

        (!has_text_entry || text_entry_matches)
            && (!has_install_state_entry || install_state_matches)
            && (!has_attach_state_entry || attach_state_matches)
    }
}

fn split_filter_entries(input: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if matches!(ch, ',' | ';' | '/' | '|') {
            push_filter_entry(&mut entries, &mut current);
        } else {
            current.push(ch);
        }
    }

    if escaped {
        current.push('\\');
    }
    push_filter_entry(&mut entries, &mut current);
    entries
}

fn push_filter_entry(entries: &mut Vec<String>, current: &mut String) {
    let entry = current.trim();
    if !entry.is_empty() {
        entries.push(entry.to_string());
    }
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::{MultiEntryFilter, split_filter_entries};

    #[test]
    fn split_filter_entries_splits_supported_separators() {
        assert_eq!(
            split_filter_entries("@ace, @rhs; @cup/@tfar| @cba"),
            vec!["@ace", "@rhs", "@cup", "@tfar", "@cba"]
        );
    }

    #[test]
    fn split_filter_entries_ignores_empty_entries() {
        assert_eq!(
            split_filter_entries(" @ace ,, ; @rhs | "),
            vec!["@ace", "@rhs"]
        );
    }

    #[test]
    fn split_filter_entries_allows_escaped_separators() {
        assert_eq!(
            split_filter_entries(r"@foo\,bar;@baz\|qux;C:\/Mods\\"),
            vec!["@foo,bar", "@baz|qux", "C:/Mods\\"]
        );
    }

    #[test]
    fn multi_entry_filter_matches_any_entry_against_any_value() {
        let filter = MultiEntryFilter::parse("@mavic3_improved, @rbs_70");

        assert!(filter.matches_any(&["C:/Mods/@RBS_70"]));
        assert!(filter.matches_any_normalized(&["@mavic3_improved", "shared"]));
        assert!(!filter.matches_any(&["@ace"]));
    }

    #[test]
    fn empty_multi_entry_filter_matches_everything() {
        let filter = MultiEntryFilter::parse(" , ; ");

        assert!(filter.matches_any(&["@ace"]));
    }

    #[test]
    fn state_keyword_matches_only_exact_state_tag() {
        let filter = MultiEntryFilter::parse("installed");

        assert!(filter.matches_with_tags(&["Some Repo"], &["installed", "attached"]));
        // "installed" must not match a "not installed" tag.
        assert!(!filter.matches_with_tags(&["Some Repo"], &["not installed", "detached"]));
    }

    #[test]
    fn exact_state_keyword_is_reserved_for_state_filtering() {
        let filter = MultiEntryFilter::parse("detached");

        // Matches via the state tag.
        assert!(filter.matches_with_tags(&["Some Repo"], &["installed", "detached"]));
        // Exact state keywords are treated as state filters, not loose text terms.
        assert!(!filter.matches_with_tags(&["Detached Pack"], &["installed", "attached"]));
    }

    #[test]
    fn multi_state_keywords_match_any_tag() {
        let filter = MultiEntryFilter::parse("not installed, attached");

        assert!(filter.matches_with_tags(&["Repo"], &["not installed", "attached"]));
        assert!(!filter.matches_with_tags(&["Repo"], &["not installed", "detached"]));
        assert!(!filter.matches_with_tags(&["Repo"], &["installed", "attached"]));
        assert!(!filter.matches_with_tags(&["Repo"], &["installed", "detached"]));
    }

    #[test]
    fn normalized_with_tags_matches_state_and_substring() {
        let filter = MultiEntryFilter::parse("installed, @ace");

        assert!(filter.matches_normalized_with_tags(&["c:/mods/@ace"], &["installed"]));
        assert!(!filter.matches_normalized_with_tags(&["some repo"], &["installed", "attached"]));
        assert!(!filter.matches_normalized_with_tags(&["c:/mods/@ace"], &["not installed"]));
        assert!(!filter.matches_normalized_with_tags(&["some repo"], &["not installed"]));
    }

    #[test]
    fn text_entries_match_any_text_while_state_entries_constrain() {
        let filter = MultiEntryFilter::parse("@ace, @rhs, installed");

        assert!(filter.matches_with_tags(&["@ACE"], &["installed", "detached"]));
        assert!(filter.matches_with_tags(&["@RHS"], &["installed", "attached"]));
        assert!(!filter.matches_with_tags(&["@CUP"], &["installed", "attached"]));
        assert!(!filter.matches_with_tags(&["@ACE"], &["not installed", "attached"]));
    }
}
