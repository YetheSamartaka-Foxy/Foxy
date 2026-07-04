use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::Galley;

use crate::core::arma3_server_query::ServerAddonQueryResult;
use crate::ui::types::{Repository, RepositoryServer};

pub type AddonInventoryEntry = (String, String, String, Option<u64>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryListSection {
    Spaces,
    Repositories,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryListRow {
    SectionLabel(RepositoryListSection),
    SpaceHeader(usize),
    FolderHeader(usize),
    Repository { repo_idx: usize, indented: bool },
}

#[derive(Debug, Default)]
pub struct RepositoryListCache {
    pub repositories_version: u64,
    pub spaces_version: u64,
    pub visual_folders_version: u64,
    pub repo_states_version: u64,
    pub repository_spaces_collapsed: bool,
    pub repositories_collapsed: bool,
    pub filter_raw: String,
    pub filter_lower: String,
    pub repository_names_lower: Vec<String>,
    pub repository_addresses_lower: Vec<String>,
    pub space_index_by_id: HashMap<String, usize>,
    pub filtered_indices: Vec<usize>,
    pub rows: Vec<RepositoryListRow>,
}

#[derive(Clone, Debug, Default)]
pub struct AddonInventoryPathCacheEntry {
    pub path: String,
    pub normalized_path_lower: String,
}

#[derive(Debug, Default)]
pub struct AddonInventoryViewCache {
    pub inventory_generation_seen: u64,
    pub addon_paths_by_name: HashMap<String, Vec<AddonInventoryPathCacheEntry>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoryAddonListKind {
    Addons,
    OptionalAddons,
}

/// Per-row text galleys for the addon list views, laid out once and reused
/// across frames.
///
/// egui's own galley cache only retains galleys laid out in the *current* frame,
/// so scrolling re-shapes every newly revealed row's text (the centered name and
/// the long path overlay) from scratch - the dominant per-frame cost while
/// scrolling long addon lists. Caching the shaped galleys here turns that
/// first-reveal shaping into a one-off cost per row, kept resident in RAM.
///
/// Galleys are laid out with [`eframe::egui::Color32::PLACEHOLDER`] and recolored
/// at paint time through the fallback color, so toggling a row's enabled /
/// favorite / client-side state never invalidates them. The name galley is
/// width-independent; the truncated path and origin galleys depend on the
/// overlay width and are dropped only when that width changes (window resize).
///
/// Slots are filled lazily as rows scroll into view, so resident memory tracks
/// the rows actually rendered rather than the full list.
#[derive(Debug, Default)]
pub struct AddonRowGalleyCache {
    name_font: f32,
    path_font: f32,
    path_wrap_width: f32,
    name_galleys: Vec<Option<Arc<Galley>>>,
    path_galleys: Vec<Option<Arc<Galley>>>,
    origin_galleys: Vec<Option<Arc<Galley>>>,
    path_widths: Vec<f32>,
    origin_widths: Vec<f32>,
}

impl AddonRowGalleyCache {
    /// Resize the per-row slots and drop every cached galley when the row count
    /// or font sizes change. Call once per frame before rendering rows.
    pub fn ensure_rows(&mut self, row_count: usize, name_font: f32, path_font: f32) -> bool {
        if self.name_galleys.len() == row_count
            && self.name_font == name_font
            && self.path_font == path_font
        {
            return false;
        }
        self.name_font = name_font;
        self.path_font = path_font;
        // Force the width-dependent galleys to rebuild on the next row.
        self.path_wrap_width = f32::NEG_INFINITY;
        self.name_galleys = vec![None; row_count];
        self.path_galleys = vec![None; row_count];
        self.origin_galleys = vec![None; row_count];
        self.path_widths = vec![f32::NEG_INFINITY; row_count];
        self.origin_widths = vec![f32::NEG_INFINITY; row_count];
        true
    }

    /// Record the current path overlay width. Width-dependent slots invalidate
    /// themselves individually, avoiding full-list cache drops during egui's
    /// layout/scrollbar width negotiation.
    pub fn ensure_path_width(&mut self, width: f32) {
        if self.path_wrap_width == width {
            return;
        }
        self.path_wrap_width = width;
    }

    pub fn name_slot(&mut self, index: usize) -> &mut Option<Arc<Galley>> {
        &mut self.name_galleys[index]
    }

    pub fn has_name(&self, index: usize) -> bool {
        self.name_galleys
            .get(index)
            .is_some_and(|slot| slot.is_some())
    }

    pub fn has_path_for_width(&self, index: usize, width: f32) -> bool {
        self.path_widths.get(index).copied() == Some(width)
            && self
                .path_galleys
                .get(index)
                .is_some_and(|slot| slot.is_some())
    }

    pub fn has_origin_for_width(&self, index: usize, width: f32) -> bool {
        self.origin_widths.get(index).copied() == Some(width)
            && self
                .origin_galleys
                .get(index)
                .is_some_and(|slot| slot.is_some())
    }

    pub fn path_slot(&mut self, index: usize) -> &mut Option<Arc<Galley>> {
        if self.path_widths[index] != self.path_wrap_width {
            self.path_widths[index] = self.path_wrap_width;
            self.path_galleys[index] = None;
        }
        &mut self.path_galleys[index]
    }

    pub fn origin_slot_for_width(&mut self, index: usize, width: f32) -> &mut Option<Arc<Galley>> {
        if self.origin_widths[index] != width {
            self.origin_widths[index] = width;
            self.origin_galleys[index] = None;
        }
        &mut self.origin_galleys[index]
    }
}

/// Per-row text galleys for the editor-mission list (repository detail view).
///
/// Same rationale as [`AddonRowGalleyCache`]: the mission list paints each row's
/// name, terrain badge and date directly with `Painter::text`, which re-shapes on
/// every scroll because egui only caches the current frame's galleys. These
/// galleys (laid out unwrapped with `Color32::PLACEHOLDER`, recolored at paint
/// time) are filled lazily per mission index and reused while scrolling.
///
/// Keyed on the mission scan timestamp ([`CachedMissionList::scanned_at`]) so any
/// rescan (rename/duplicate/delete or the 30s TTL refresh) drops stale galleys,
/// plus the three font sizes so a font-size change rebuilds them.
#[derive(Debug, Default)]
pub struct MissionRowGalleyCache {
    scanned_at: Option<Instant>,
    name_font: f32,
    world_font: f32,
    date_font: f32,
    name_galleys: Vec<Option<Arc<Galley>>>,
    world_galleys: Vec<Option<Arc<Galley>>>,
    date_galleys: Vec<Option<Arc<Galley>>>,
}

impl MissionRowGalleyCache {
    /// Resize the per-row slots and drop every cached galley when the mission
    /// scan or any font size changes. Call once per frame before rendering rows.
    pub fn ensure(
        &mut self,
        scanned_at: Option<Instant>,
        mission_count: usize,
        name_font: f32,
        world_font: f32,
        date_font: f32,
    ) {
        if self.name_galleys.len() == mission_count
            && self.scanned_at == scanned_at
            && self.name_font == name_font
            && self.world_font == world_font
            && self.date_font == date_font
        {
            return;
        }
        self.scanned_at = scanned_at;
        self.name_font = name_font;
        self.world_font = world_font;
        self.date_font = date_font;
        self.name_galleys = vec![None; mission_count];
        self.world_galleys = vec![None; mission_count];
        self.date_galleys = vec![None; mission_count];
    }

    pub fn name_slot(&mut self, index: usize) -> &mut Option<Arc<Galley>> {
        &mut self.name_galleys[index]
    }

    pub fn world_slot(&mut self, index: usize) -> &mut Option<Arc<Galley>> {
        &mut self.world_galleys[index]
    }

    pub fn date_slot(&mut self, index: usize) -> &mut Option<Arc<Galley>> {
        &mut self.date_galleys[index]
    }
}

/// Generic lazy, frame-persistent galley cache for the rows of any scrollable
/// list, generalizing [`AddonRowGalleyCache`] / [`MissionRowGalleyCache`].
///
/// egui only retains galleys laid out in the *current* frame, so any list that
/// renders just its visible rows - a virtualized `ScrollArea::show_rows`, or a
/// `ScrollArea::show` that skips off-screen rows via `is_rect_visible` - must
/// re-shape every row that scrolls into view. (Lists that lay out *all* their
/// rows every frame do not have this problem; egui's per-frame cache already
/// covers them and this cache would be dead weight.) This shapes each row's text
/// once and reuses the `Arc<Galley>` across frames.
///
/// The caller supplies a `generation` (bump whenever the row *contents* change)
/// and a `fingerprint` (fold in every layout input other than the row index:
/// font sizes, wrap widths, and any color baked into the galley - see
/// [`crate::ui::views::galley_cache::fingerprint`]). When either changes - or the
/// row / column count changes - every slot is dropped. Each row owns `columns`
/// slots, filled lazily as rows scroll into view, so resident memory tracks the
/// rows actually rendered. The slots are filled and painted with the helpers in
/// [`crate::ui::views::galley_cache`].
#[derive(Debug, Default)]
pub struct ListGalleyCache {
    generation: u64,
    fingerprint: u64,
    rows: usize,
    columns: usize,
    slots: Vec<Option<Arc<Galley>>>,
}

impl ListGalleyCache {
    /// Resize and clear the slots when the row / column count, content
    /// `generation`, or layout `fingerprint` changes. Call once per frame before
    /// rendering rows.
    pub fn ensure(&mut self, rows: usize, columns: usize, generation: u64, fingerprint: u64) {
        if self.rows == rows
            && self.columns == columns
            && self.generation == generation
            && self.fingerprint == fingerprint
        {
            return;
        }
        self.rows = rows;
        self.columns = columns;
        self.generation = generation;
        self.fingerprint = fingerprint;
        self.slots = vec![None; rows.saturating_mul(columns)];
    }

    /// Flat slot index for a `(row, column)` cell.
    const fn slot_index(columns: usize, row: usize, column: usize) -> usize {
        row * columns + column
    }

    /// Mutable galley slot for `(row, column)`.
    pub fn slot(&mut self, row: usize, column: usize) -> &mut Option<Arc<Galley>> {
        debug_assert!(
            row < self.rows,
            "row {row} out of range (rows {})",
            self.rows
        );
        debug_assert!(
            column < self.columns,
            "column {column} out of range (columns {})",
            self.columns
        );
        let index = Self::slot_index(self.columns, row, column);
        &mut self.slots[index]
    }
}

#[derive(Debug, Default)]
pub struct RepositoryAddonListCache {
    pub repo_index: Option<usize>,
    pub selected_profile: Option<String>,
    pub inventory_generation_seen: u64,
    pub repo_path_normalized: String,
    pub source_names: Vec<String>,
    pub normalized_names: Vec<String>,
    pub enabled_by_source: Vec<bool>,
    pub favorite_by_source: Vec<bool>,
    pub client_side_by_source: Vec<bool>,
    pub forced_client_side_by_source: Vec<bool>,
    pub remote_size_bytes_by_source: Vec<u64>,
    pub sorted_indices: Vec<usize>,
    pub preferred_paths: Vec<Option<String>>,
    pub filtered_indices: Vec<usize>,
    pub filter_lower: String,
    pub state_filter: String,
    pub favorites_only_filter: bool,
    pub client_side_only_filter: bool,
    pub filters_dirty: bool,
    pub galleys: AddonRowGalleyCache,
    /// Cursor into `filtered_indices` for the incremental galley prewarm, so the
    /// name/path galleys of off-screen rows are shaped ahead of the scroll
    /// instead of on first reveal. Reset whenever the rows, filter, or overlay
    /// width change. See `prewarm_repository_addon_galleys`.
    pub galley_prewarm_cursor: usize,
    pub galley_prewarm_path_width: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct ExternalAddonRowCache {
    pub addon_name: String,
    pub path: String,
    pub origin: String,
    pub addon_name_lower: String,
    pub path_lower: String,
    pub origin_lower: String,
    pub path_lookup_key: String,
    pub local_size_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct RepositoryExternalAddonsListCache {
    pub repo_index: Option<usize>,
    pub selected_profile: Option<String>,
    pub include_steam_addons: bool,
    pub inventory_generation_seen: u64,
    pub scope_key: String,
    pub local_addon_names: Vec<String>,
    pub local_optional_addon_names: Vec<String>,
    pub remote_client_side_addon_names: Vec<String>,
    pub rows: Vec<ExternalAddonRowCache>,
    pub origin_options: Vec<String>,
    pub collapsed_origins: HashSet<String>,
    pub enabled_by_row: Vec<bool>,
    pub favorite_by_row: Vec<bool>,
    pub client_side_by_row: Vec<bool>,
    pub forced_client_side_by_row: Vec<bool>,
    pub filtered_indices: Vec<usize>,
    pub grouped_filtered_indices: Vec<(String, Vec<usize>)>,
    pub enabled_count: usize,
    pub enabled_size_bytes: u64,
    pub filtered_size_bytes: u64,
    pub total_size_bytes: u64,
    pub filter_lower: String,
    pub origin_filter: String,
    pub state_filter: String,
    pub favorites_only_filter: bool,
    pub client_side_only_filter: bool,
    pub filters_dirty: bool,
    pub galleys: AddonRowGalleyCache,
    pub galley_prewarm_cursor: usize,
    pub galley_prewarm_path_width: Option<f32>,
    pub galley_prewarm_include_origin: bool,
}

#[derive(Debug)]
pub struct RepositorySettingsAddonPreloadResult {
    pub repo_index: usize,
    pub inventory_generation: u64,
    pub addons: Vec<AddonInventoryEntry>,
}

#[derive(Debug, Default)]
pub struct RepositoryAddonSizeLoadResult {
    pub sizes_by_repo_and_addon: HashMap<(String, String), u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinPreflightAddonOrigin {
    Required,
    Optional,
    External,
}

#[derive(Clone, Debug)]
pub struct JoinPreflightAddonSuggestion {
    pub addon_name: String,
    pub origin: JoinPreflightAddonOrigin,
    pub reported_name: String,
    pub confidence: JoinPreflightMatchConfidence,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct JoinPreflightKnownRemoteAddon {
    pub reported_name: String,
    pub addon_name: String,
    pub repository_name: String,
    pub repository_url: String,
    pub repository_path: String,
    pub available: bool,
    pub confidence: JoinPreflightMatchConfidence,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct JoinPreflightAmbiguousAddon {
    pub reported_name: String,
    pub candidates: Vec<JoinPreflightAddonSuggestion>,
    pub selected_candidate: Option<usize>,
}

/// An external addon that is enabled for this repository but whose configured
/// path can no longer be resolved on disk. These are silently skipped by the
/// launcher, so the preflight surfaces them as a warning instead of letting the
/// user join believing they are loaded.
#[derive(Clone, Debug)]
pub struct JoinPreflightUnavailableAddon {
    pub addon_name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct PendingJoinPreflightState {
    pub repo_name: String,
    pub server: RepositoryServer,
    pub original_repository: Repository,
    pub suggestions: Vec<JoinPreflightAddonSuggestion>,
    pub ambiguous: Vec<JoinPreflightAmbiguousAddon>,
    pub known_remote: Vec<JoinPreflightKnownRemoteAddon>,
    pub extra_enabled: Vec<JoinPreflightAddonSuggestion>,
    /// Enabled external addons whose configured path no longer resolves on disk.
    /// Informational only: the launcher skips them, so the modal warns about
    /// them rather than letting them disappear silently.
    pub unavailable_enabled: Vec<JoinPreflightUnavailableAddon>,
    /// Repository ships TeamSpeak plugins and the running-check is enabled.
    pub ts3_required: bool,
    /// Latest result of the TeamSpeak-running process check.
    pub ts3_running: bool,
    /// The Steam-running check is enabled (Steam is required to launch Arma 3).
    pub steam_required: bool,
    /// Latest result of the Steam-running process check.
    pub steam_running: bool,
    /// This modal was opened from the plain "Launch" button rather than a
    /// "Join" action, so there is no server to connect to. The `server` field
    /// holds a placeholder and the launch must not pass connection params.
    pub launch_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinPreflightMatchConfidence {
    ExactNormalizedName,
}

#[derive(Clone, Debug)]
pub struct PendingJoinPreflightQuery {
    pub repo_name: String,
    pub server: RepositoryServer,
    pub original_repository: Repository,
}

/// A Join request waiting on a background server-status (A2S) query. The status
/// query does DNS resolution plus a UDP round-trip, so it runs off the UI
/// thread; the join decision resumes once a fresh status arrives.
#[derive(Clone, Debug)]
pub struct PendingJoinStatusQuery {
    pub repo_name: String,
    pub server: RepositoryServer,
    pub effective_repository: Repository,
    /// Server-status cache key `(address, port)` this query is keyed on.
    pub key: (String, String),
    pub started_at: Instant,
}

#[derive(Clone, Debug)]
pub struct JoinPreflightCacheEntry {
    pub result: ServerAddonQueryResult,
    pub display_names: crate::core::addon_metadata::AddonDisplayNameSnapshot,
    pub cached_at: Instant,
}

#[derive(Clone, Debug)]
pub struct JoinPreflightQueryResult {
    pub address: String,
    pub port: u16,
    pub display_names: crate::core::addon_metadata::AddonDisplayNameSnapshot,
    pub result: Result<ServerAddonQueryResult, String>,
}

#[cfg(test)]
mod tests {
    use super::ListGalleyCache;

    #[test]
    fn slot_index_is_row_major() {
        assert_eq!(ListGalleyCache::slot_index(3, 0, 0), 0);
        assert_eq!(ListGalleyCache::slot_index(3, 0, 2), 2);
        assert_eq!(ListGalleyCache::slot_index(3, 1, 0), 3);
        assert_eq!(ListGalleyCache::slot_index(3, 2, 1), 7);
    }

    #[test]
    fn ensure_allocates_one_slot_per_cell() {
        let mut cache = ListGalleyCache::default();
        cache.ensure(4, 2, 1, 10);
        assert_eq!(cache.slots.len(), 8);
        assert!(cache.slots.iter().all(Option::is_none));
    }

    #[test]
    fn ensure_is_a_noop_when_inputs_are_unchanged() {
        let mut cache = ListGalleyCache::default();
        cache.ensure(2, 1, 1, 10);
        // A filled slot must survive an identical `ensure` so scrolling does not
        // re-shape rows every frame.
        *cache.slot(0, 0) = None;
        let ptr = cache.slots.as_ptr();
        cache.ensure(2, 1, 1, 10);
        assert_eq!(cache.slots.as_ptr(), ptr, "slots were reallocated");
    }

    #[test]
    fn ensure_rebuilds_when_generation_or_fingerprint_changes() {
        let mut cache = ListGalleyCache::default();
        cache.ensure(2, 1, 1, 10);
        cache.ensure(2, 1, 2, 10);
        assert_eq!(cache.slots.len(), 2);
        cache.ensure(2, 1, 2, 11);
        assert_eq!(cache.slots.len(), 2);
        // Shrinking the row count rebuilds too.
        cache.ensure(1, 1, 2, 11);
        assert_eq!(cache.slots.len(), 1);
    }
}
