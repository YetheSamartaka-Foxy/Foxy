use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, UserData, Vec2,
    ViewportCommand,
};
use rand::{RngExt, distr::Alphanumeric, rng};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::api;
use crate::ui::app::{AddonInventoryEntry, Foxy, FoxyView, MemoryDiagnosticsSample};
use crate::ui::types::{
    MAX_UI_SCALE_PERCENT, MIN_UI_SCALE_PERCENT, RepoState, RepositorySelection,
    RepositorySettingsTab,
};

const SESSION_FILE_NAME: &str = "agent-gui-session.json";
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);
/// Extra time on top of the expected render window before a parked `settle`
/// gives up, so a momentarily idle frame clock cannot wedge the response.
const SETTLE_SLACK: Duration = Duration::from_secs(3);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default)]
pub struct AgentGuiLaunchConfig {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiSession {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub session_file: PathBuf,
}

#[derive(Debug)]
pub struct AgentGuiRuntime {
    rx: Receiver<AgentGuiUiRequest>,
    pub session: AgentGuiSession,
    started_at: Instant,
    pending_screenshots: Vec<PendingScreenshot>,
    pending_waits: Vec<PendingWait>,
    pending_settles: Vec<PendingSettle>,
    /// In-flight `drag` gestures, advanced one event per rendered frame.
    pending_drags: Vec<PendingDrag>,
    /// In-flight server-side `batch` pipelines, resumed across frames.
    pending_batches: Vec<PendingBatch>,
    /// Recent observations kept for `diff` (ring; newest last). Pushed by every
    /// `snapshot`/`diff` so an agent can diff against `last` or `frame:<n>`.
    diff_baselines: VecDeque<AgentGuiSnapshot>,
    /// Named UI-state checkpoints for `checkpoint`/`restore` (UI state only).
    checkpoints: HashMap<String, Value>,
    /// Append-only semantic UI-event ring for `events`, with a monotonic
    /// generation for incremental tailing (mirrors the `logs` idiom).
    events: VecDeque<AgentUiEvent>,
    event_generation: u64,
    /// Previous-frame state used to derive transition events in `poll`.
    prev_view: Option<String>,
    prev_modals: Vec<String>,
    prev_focused: Option<String>,
    prev_toast: Option<String>,
    prev_download_active: bool,
    prev_download_finished: bool,
    /// Active renderer backend captured at startup (`wgpu`/`glow`), surfaced by
    /// the `health` command. `None` until `Foxy::new` records it.
    pub active_renderer: Option<&'static str>,
    /// When true, animation time is frozen and blink/hover/spinner animations
    /// are disabled so screenshots are byte-stable (`stable-render`).
    pub stable_render: bool,
}

/// Maximum stored observations for `diff` and maximum buffered `events`.
const DIFF_BASELINE_CAP: usize = 32;
const EVENT_RING_CAP: usize = 500;

/// A `drag` gesture parked across frames: emits a pointer-down, `steps`
/// interpolated moves, then a pointer-up - one event per rendered frame so egui
/// classifies it as a real drag (a single in-frame down→move→up does not).
#[derive(Debug)]
struct PendingDrag {
    command: AgentGuiCommand,
    from: Pos2,
    to: Pos2,
    button: PointerButton,
    steps: u32,
    /// 0 = emit down; 1..=steps = emit interpolated move; steps+1 = emit up.
    phase: u32,
    /// Frame number at which the next event should fire.
    next_frame: u64,
    response_tx: Sender<AgentGuiResponse>,
    deadline: Instant,
    requested_at: Instant,
}

/// A server-side `batch` pipeline parked between steps. Each step runs through
/// the normal request handler with an internal response channel; a step that
/// parks stores its `step_rx` here and the batch resumes when it fires.
#[derive(Debug)]
struct PendingBatch {
    steps: Vec<AgentGuiCommand>,
    next_index: usize,
    results: Vec<Value>,
    stop_on_error: bool,
    all_ok: bool,
    response_tx: Sender<AgentGuiResponse>,
    requested_at: Instant,
    deadline: Instant,
    /// Set while the current step is parked, awaiting its response.
    step_rx: Option<Receiver<AgentGuiResponse>>,
}

/// One semantic UI event for the `events` ring buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AgentUiEvent {
    generation: u64,
    kind: String,
    detail: Value,
}

#[derive(Debug)]
struct PendingScreenshot {
    request_id: u64,
    output: PathBuf,
    response_tx: Sender<AgentGuiResponse>,
    requested_at: Instant,
    /// Node rects captured at request time when `--annotate` was set, so the
    /// completion path can draw the overlay and emit the sidecar JSON.
    annotation: Option<Vec<AgentGuiNode>>,
}

#[derive(Debug)]
struct PendingWait {
    request_id: u64,
    command: AgentGuiCommand,
    condition: AgentGuiWaitCondition,
    response_tx: Sender<AgentGuiResponse>,
    deadline: Instant,
    requested_at: Instant,
}

/// A `settle`/`nav` response parked until the frame clock advances to
/// `target_frame`, at which point the post-input snapshot is returned.
#[derive(Debug)]
struct PendingSettle {
    command: AgentGuiCommand,
    target_frame: u64,
    response_tx: Sender<AgentGuiResponse>,
    deadline: Instant,
    requested_at: Instant,
    /// When set, include the resolved focused-widget name in the response (used
    /// by `nav` so the caller learns where focus landed).
    report_focus: bool,
}

#[derive(Debug)]
struct AgentGuiUiRequest {
    request_id: u64,
    command: AgentGuiCommand,
    response_tx: Sender<AgentGuiResponse>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiWireRequest {
    pub token: String,
    pub command: AgentGuiCommand,
}

/// Keyboard/pointer modifier state shared by `key`, `click`, `scroll`, and the
/// mouse-button commands. All fields default to `false` so older clients (and
/// hand-written JSON) can omit the block entirely.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct AgentGuiModifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub command: bool,
}

impl AgentGuiModifiers {
    fn to_egui(self) -> Modifiers {
        Modifiers {
            alt: self.alt,
            ctrl: self.ctrl,
            shift: self.shift,
            mac_cmd: self.command,
            // On non-mac platforms egui mirrors `command` onto Ctrl, so map
            // `--ctrl` to both for shortcuts that match either field.
            command: self.ctrl || self.command,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum AgentGuiCommand {
    Status,
    OpenView {
        view: String,
        repository_index: Option<usize>,
        tab: Option<String>,
    },
    Snapshot {
        /// Project only these top-level snapshot keys server-side (e.g.
        /// `view`, `fps`, `busy`). Empty/None returns the full snapshot.
        #[serde(default)]
        fields: Option<Vec<String>>,
        /// Return `{changed:false, frame}` when no new frame has rendered since
        /// this cumulative frame number, so polling loops stop re-paying for an
        /// unchanged snapshot. The cursor is egui's real `cumulative_frame_nr`.
        #[serde(default)]
        since_frame: Option<u64>,
    },
    Text {
        contains: Option<String>,
        limit: Option<usize>,
    },
    Find {
        text: Option<String>,
        role: Option<String>,
        id: Option<String>,
        visible_only: bool,
    },
    Click {
        text: Option<String>,
        id: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
        #[serde(default)]
        modifiers: AgentGuiModifiers,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        double: bool,
    },
    Scroll {
        id: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
        dx: f32,
        dy: f32,
        #[serde(default)]
        modifiers: AgentGuiModifiers,
    },
    Hover {
        id: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
    },
    MouseDown {
        id: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
        #[serde(default)]
        modifiers: AgentGuiModifiers,
        #[serde(default)]
        button: Option<String>,
    },
    MouseUp {
        id: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
        #[serde(default)]
        modifiers: AgentGuiModifiers,
        #[serde(default)]
        button: Option<String>,
    },
    Key {
        key: String,
        #[serde(default)]
        modifiers: AgentGuiModifiers,
    },
    Type {
        text: String,
    },
    Screenshot {
        output: PathBuf,
        /// Overlay each known node's rect + id and the pointer position, and
        /// write a sidecar `<output>.nodes.json` mapping rect → id, so a
        /// misfiring coordinate click can be debugged from one artifact.
        #[serde(default)]
        annotate: bool,
    },
    Fps,
    Wait {
        condition: AgentGuiWaitCondition,
        timeout_ms: u64,
    },
    Logs {
        #[serde(default)]
        level: Option<String>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        since_generation: Option<u64>,
    },
    /// Structured list of the configured repositories (no UI navigation
    /// required), including sync state and pending-update counts.
    Repositories {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Structured addon rows for a repository tab. Addon rows are not semantic
    /// nodes, so this is the only way to read their names/enabled/size state.
    Addons {
        #[serde(default)]
        repository_index: Option<usize>,
        #[serde(default)]
        tab: Option<String>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        enabled_only: bool,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Dump the live, effective settings the running app currently holds.
    Settings,
    /// Live download/sync/recheck progress and the reasons the app is busy.
    Progress,
    /// Set the global UI scale (percent of native scale), as the settings
    /// slider does. Useful for reproducing high-DPI relayout/multi-pass paths.
    Scale {
        percent: u16,
    },
    /// Resize the OS window to the given logical inner size (points). Useful for
    /// reproducing large-window relayout/multi-pass paths.
    Resize {
        width: f32,
        height: f32,
    },
    /// Structured launch profiles for a repository (name, selected flag, launch
    /// flags, and addon/optional/external override counts). Profiles are not
    /// exposed as semantic nodes.
    Profiles {
        #[serde(default)]
        repository_index: Option<usize>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Cached editor missions for the currently viewed repository (name, folder,
    /// terrain). Mirrors the in-memory `cached_missions` list.
    Missions {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Repository spaces with attached-repository counts, the selected space, and
    /// any in-flight bulk-action progress.
    Spaces {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// The last completed download summary (counts, bytes, patch savings, stage
    /// durations) and, optionally, the per-sample telemetry series.
    DownloadSummary {
        #[serde(default)]
        include_telemetry: bool,
    },
    /// The current user-feedback toast (message, kind, age, remaining), or
    /// `present: false` when none is showing.
    Toasts,
    /// Mutate a single live setting on the running app and observe the UI react.
    /// The read/write complement to the read-only `settings` fetch; clamps and
    /// validates like the offline `settings set` CLI.
    SetSetting {
        key: String,
        value: String,
    },
    /// Build version + git hash, build kind, active renderer backend, and
    /// whether this is an agent-gui build. The natural first call of a session
    /// and a single CI gate for client/server version mismatch.
    Health,
    /// Set keyboard focus on a named text field (or clear focus). Surfaces the
    /// resolved egui id; the focused widget also appears in `snapshot.focused`.
    Focus {
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        clear: bool,
    },
    /// Send N Tab / Shift+Tab presses to traverse focus the keyboard way, then
    /// (after one settle frame) report the widget that ends up focused.
    Nav {
        #[serde(default = "default_nav_count")]
        count: u32,
        #[serde(default)]
        reverse: bool,
    },
    /// Focus + clear + set a named text field in one step, by writing the
    /// backing app state directly (more reliable than focus-then-type).
    Fill {
        target: String,
        value: String,
    },
    /// Read the current repository/addon/mission list filter values.
    Filters,
    /// Write one named list filter and request a repaint. The reliable way to
    /// drive the addon-list scroll/galley recipes (filter to a known subset).
    SetFilter {
        name: String,
        value: String,
    },
    /// Non-destructive UI selection: highlight/view a repository, server,
    /// mission, or space without coordinates. Never mutates core data.
    Select {
        #[serde(default)]
        repository: Option<usize>,
        #[serde(default)]
        server: Option<usize>,
        #[serde(default)]
        mission: Option<usize>,
        #[serde(default)]
        space: Option<String>,
    },
    /// Window/tray lifecycle: minimize/restore/maximize/focus/hide-to-tray/show.
    Window {
        action: String,
    },
    /// Park the response until `frames` more frames have actually rendered, then
    /// return the post-input snapshot - a synchronization ack for queued input.
    Settle {
        #[serde(default = "default_settle_frames")]
        frames: u64,
    },
    /// Toggle stable-render mode: zero egui's animation time and disable caret
    /// blink / hover fades / spinner animation so screenshots are byte-stable.
    StableRender {
        on: bool,
    },
    /// Declarative assertion over a single observed field; returns ok/fail with
    /// the observed-vs-expected values so scripts fail fast with a clear diff.
    Assert {
        field: String,
        #[serde(default)]
        equals: Option<String>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        repository_index: Option<usize>,
    },
    /// Secondary-click a target to open its egui context menu (popups are not
    /// semantic nodes, so this is a coordinate/id helper).
    ContextMenu {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
    },
    /// Activate an open context-menu / popup entry by its visible label text.
    MenuSelect {
        item: String,
    },
    /// Global cross-folder addon inventory (`cached_all_addons`): which addons
    /// are shared across repositories/folders, filterable and size-summed.
    Inventory {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        folder: Option<String>,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// The planned update set *before* a sync (`pending_update_cache`), so an
    /// agent can assert the plan, kick a download, then diff plan vs result.
    PendingUpdates {
        #[serde(default)]
        repository_index: Option<usize>,
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        include_files: bool,
    },
    /// The self-update flow status (`app_update_status`) plus the mode/url
    /// settings, so check → available → download can be driven and observed.
    AppUpdate,
    /// The latest memory-diagnostics sample (working-set / private / tracked
    /// bytes, per-bucket breakdown) and texture-tracking maps; optionally the
    /// full series. Turns texture-leak/memory-growth work into an assertion.
    Memory {
        #[serde(default)]
        history: bool,
        #[serde(default)]
        textures: bool,
    },
    /// OS-level Arma 3 *player* profiles (`detected_arma3_profiles`) that drive
    /// the `-profiles` launch argument, distinct from Foxy launch profiles.
    ArmaProfiles,
    /// Addon-backup records (`backup_manager_records`): addon, timestamp, size,
    /// count-per-addon, so an agent can assert an update produced a backup.
    Backups {
        #[serde(default)]
        contains: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Semantic app-action: drive Foxy by named intent rather than by pixels.
    /// `list_actions` enumerates the registry; otherwise `action` is run with
    /// `params`, gated by `allow_destructive` for core/disk mutations.
    Invoke {
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        params: Value,
        #[serde(default)]
        allow_destructive: bool,
        #[serde(default)]
        list_actions: bool,
    },
    /// Server-side pipeline: run an array of commands in order on the UI thread,
    /// resuming across frames for any step that parks (settle/wait/screenshot/
    /// drag), and return an array of per-step responses in one round-trip.
    Batch {
        steps: Vec<AgentGuiCommand>,
        #[serde(default = "default_stop_on_error")]
        stop_on_error: bool,
    },
    /// Field-level delta between the current observation and a stored baseline
    /// (`last`, or `frame:<n>` captured by an earlier snapshot/diff).
    Diff {
        #[serde(default = "default_diff_baseline")]
        baseline: String,
    },
    /// First-class drag gesture: schedules a down → interpolated moves → up
    /// sequence across real frames (egui only classifies multi-frame drags).
    Drag {
        #[serde(default)]
        from_id: Option<String>,
        #[serde(default)]
        from_x: Option<f32>,
        #[serde(default)]
        from_y: Option<f32>,
        #[serde(default)]
        to_id: Option<String>,
        #[serde(default)]
        to_x: Option<f32>,
        #[serde(default)]
        to_y: Option<f32>,
        #[serde(default = "default_drag_steps")]
        steps: u32,
        #[serde(default)]
        button: Option<String>,
    },
    /// One JMESPath query over the union of the structured state fetches
    /// (snapshot + settings + progress + repositories + spaces + …). Read-only.
    Query {
        expr: String,
    },
    /// Save the serializable UI-state subset under a name for later `restore`
    /// (or `list` the saved names). UI state only - never core/disk state.
    Checkpoint {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        list: bool,
    },
    /// Roll the UI back to a named `checkpoint`. UI state only.
    Restore {
        name: String,
    },
    /// Deep single-node introspection: relationships + interaction flags for one
    /// node by id, or the hit-test winner at a coordinate.
    Element {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
    },
    /// Recent semantic UI events (the causal counterpart to `logs`): clicks,
    /// keys, view/modal/focus transitions, toasts, download-state changes.
    Events {
        #[serde(default)]
        kinds: Option<Vec<String>>,
        #[serde(default)]
        since: Option<u64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Virtual time control for determinism: advance/freeze/resume the logical
    /// clock the UI timers read (toast expiry, etc.).
    Clock {
        action: String,
        #[serde(default)]
        ms: Option<u64>,
    },
    /// Native file/folder picker automation: pre-register the response for the
    /// next picker (`expect`), report picker state (`pending`), or clear it.
    Dialog {
        action: String,
        #[serde(default)]
        path: Option<PathBuf>,
        #[serde(default)]
        cancel: bool,
    },
    Close,
}

/// Default Tab presses for `nav` when the client omits a count.
fn default_nav_count() -> u32 {
    1
}

/// Default frames to wait for `settle` when the client omits a count.
fn default_settle_frames() -> u64 {
    2
}

/// `batch` stops on the first failing step unless told otherwise.
fn default_stop_on_error() -> bool {
    true
}

/// `diff` compares against the most recent stored observation by default.
fn default_diff_baseline() -> String {
    "last".to_string()
}

/// Default interpolated move count for a `drag` gesture.
fn default_drag_steps() -> u32 {
    6
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AgentGuiWaitCondition {
    Text {
        text: String,
    },
    View {
        view: String,
    },
    Idle,
    Modal {
        open: bool,
    },
    /// Satisfied once a user-feedback toast whose message contains `text` is
    /// showing. Pairs with the `toasts` fetch.
    Toast {
        text: String,
    },
    /// Satisfied once the named background-work flag (see `busy_reasons`) is no
    /// longer set - e.g. wait out a single `core-sync` without going fully
    /// idle.
    BusyReasonCleared {
        reason: String,
    },
    /// Satisfied once a download has completed (`download_finished`).
    DownloadComplete,
    /// Satisfied once the smoothed FPS estimate is at or above `fps`.
    FpsAbove {
        fps: f32,
    },
    /// Satisfied once a known semantic node with `id` is on-screen.
    NodeVisible {
        id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiResponse {
    pub ok: bool,
    pub command: String,
    pub view: String,
    pub elapsed_ms: u128,
    pub data: Value,
    pub errors: Vec<AgentGuiError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AgentGuiPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiNode {
    pub id: String,
    pub role: String,
    pub text: String,
    pub enabled: bool,
    pub focused: bool,
    pub rect: Option<AgentGuiRect>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentGuiSnapshot {
    pub view: String,
    pub update_modal_open: bool,
    pub fps: f32,
    pub startup_frame_rendered: bool,
    pub busy: bool,
    pub active_modal_count: usize,
    /// Stable kebab-case names of the modals currently considered open. Lets an
    /// agent assert *which* dialog appeared rather than only how many.
    pub active_modals: Vec<String>,
    /// Latest known pointer position in egui logical points, if the cursor is
    /// over the window. Useful for confirming `hover`/`scroll` targeting.
    pub pointer: Option<AgentGuiPoint>,
    /// The currently keyboard-focused widget: a friendly name for a registered
    /// agent target (e.g. `add-repository-input`), else the raw egui id, else
    /// `null` when nothing has focus. Drive it with `focus`/`nav`; `type`/`key`
    /// land on whatever this reports.
    pub focused: Option<String>,
    pub repositories_count: usize,
    pub selected_repository: Option<usize>,
    pub settings_tab: Option<String>,
    pub repository_settings_tab: Option<String>,
    pub frame: u64,
    /// egui's cumulative pass count. Larger than `frame` whenever egui ran
    /// extra layout passes (multi-pass via `request_discard`). Diff this and
    /// `frame` across two reads while scrolling to detect the per-frame
    /// multi-pass that produces "changed id between passes" warnings.
    pub cumulative_pass_nr: u64,
    pub pixels_per_point: f32,
    /// egui zoom factor (`pixels_per_point` = native scale × this). Mirrors the
    /// global UI-scale setting; drive it with the `scale` command.
    pub zoom_factor: f32,
    /// Stable kebab-case names of the background-work flags currently set, so an
    /// agent can see *why* `busy` is true (empty when idle).
    pub busy_reasons: Vec<String>,
    pub content_rect: AgentGuiRect,
    pub texts: Vec<String>,
    pub nodes: Vec<AgentGuiNode>,
}

impl AgentGuiRuntime {
    fn new(rx: Receiver<AgentGuiUiRequest>, session: AgentGuiSession) -> Self {
        Self {
            rx,
            session,
            started_at: Instant::now(),
            pending_screenshots: Vec::new(),
            pending_waits: Vec::new(),
            pending_settles: Vec::new(),
            pending_drags: Vec::new(),
            pending_batches: Vec::new(),
            diff_baselines: VecDeque::new(),
            checkpoints: HashMap::new(),
            events: VecDeque::new(),
            event_generation: 0,
            prev_view: None,
            prev_modals: Vec::new(),
            prev_focused: None,
            prev_toast: None,
            prev_download_active: false,
            prev_download_finished: false,
            active_renderer: None,
            stable_render: false,
        }
    }

    /// Append a semantic UI event to the ring, returning its generation.
    fn record_event(&mut self, kind: &str, detail: Value) {
        self.event_generation += 1;
        self.events.push_back(AgentUiEvent {
            generation: self.event_generation,
            kind: kind.to_string(),
            detail,
        });
        while self.events.len() > EVENT_RING_CAP {
            self.events.pop_front();
        }
    }

    /// Store an observation for later `diff` lookups (capped ring).
    fn push_diff_baseline(&mut self, snapshot: AgentGuiSnapshot) {
        self.diff_baselines.push_back(snapshot);
        while self.diff_baselines.len() > DIFF_BASELINE_CAP {
            self.diff_baselines.pop_front();
        }
    }
}

impl AgentGuiCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::OpenView { .. } => "open-view",
            Self::Snapshot { .. } => "snapshot",
            Self::Text { .. } => "text",
            Self::Find { .. } => "find",
            Self::Click { .. } => "click",
            Self::Scroll { .. } => "scroll",
            Self::Hover { .. } => "hover",
            Self::MouseDown { .. } => "mouse-down",
            Self::MouseUp { .. } => "mouse-up",
            Self::Key { .. } => "key",
            Self::Type { .. } => "type",
            Self::Screenshot { .. } => "screenshot",
            Self::Fps => "fps",
            Self::Wait { .. } => "wait",
            Self::Logs { .. } => "logs",
            Self::Repositories { .. } => "repositories",
            Self::Addons { .. } => "addons",
            Self::Settings => "settings",
            Self::Progress => "progress",
            Self::Scale { .. } => "scale",
            Self::Resize { .. } => "resize",
            Self::Profiles { .. } => "profiles",
            Self::Missions { .. } => "missions",
            Self::Spaces { .. } => "spaces",
            Self::DownloadSummary { .. } => "download-summary",
            Self::Toasts => "toasts",
            Self::SetSetting { .. } => "set-setting",
            Self::Health => "health",
            Self::Focus { .. } => "focus",
            Self::Nav { .. } => "nav",
            Self::Fill { .. } => "fill",
            Self::Filters => "filters",
            Self::SetFilter { .. } => "set-filter",
            Self::Select { .. } => "select",
            Self::Window { .. } => "window",
            Self::Settle { .. } => "settle",
            Self::StableRender { .. } => "stable-render",
            Self::Assert { .. } => "assert",
            Self::ContextMenu { .. } => "context-menu",
            Self::MenuSelect { .. } => "menu-select",
            Self::Inventory { .. } => "inventory",
            Self::PendingUpdates { .. } => "pending-updates",
            Self::AppUpdate => "app-update",
            Self::Memory { .. } => "memory",
            Self::ArmaProfiles => "arma-profiles",
            Self::Backups { .. } => "backups",
            Self::Invoke { .. } => "invoke",
            Self::Batch { .. } => "batch",
            Self::Diff { .. } => "diff",
            Self::Drag { .. } => "drag",
            Self::Query { .. } => "query",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Restore { .. } => "restore",
            Self::Element { .. } => "element",
            Self::Events { .. } => "events",
            Self::Clock { .. } => "clock",
            Self::Dialog { .. } => "dialog",
            Self::Close => "close",
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            Self::Screenshot { .. } => SCREENSHOT_TIMEOUT,
            Self::Wait { timeout_ms, .. } => {
                Duration::from_millis(*timeout_ms).saturating_add(Duration::from_secs(2))
            }
            // `nav` parks for one settle frame; `settle` parks for N frames.
            // Give both a generous ceiling above the render cadence.
            Self::Nav { .. } => DEFAULT_COMMAND_TIMEOUT,
            Self::Settle { frames } => Duration::from_millis(frames.saturating_mul(50).max(200))
                .saturating_add(SETTLE_SLACK),
            // A batch is bounded by the sum of its steps' timeouts so a long
            // `wait`/`download-complete` step inside it does not trip the client.
            Self::Batch { steps, .. } => steps
                .iter()
                .map(|step| step.timeout())
                .fold(Duration::ZERO, |acc, step| acc.saturating_add(step))
                .saturating_add(SETTLE_SLACK)
                .max(DEFAULT_COMMAND_TIMEOUT),
            // A drag spreads its events across `steps + 2` frames; allow for the
            // render cadence plus slack.
            Self::Drag { steps, .. } => Duration::from_millis(
                u64::from(steps.saturating_add(2))
                    .saturating_mul(50)
                    .max(200),
            )
            .saturating_add(SETTLE_SLACK),
            _ => DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

impl AgentGuiResponse {
    fn ok(command: &AgentGuiCommand, view: &str, started_at: Instant, data: Value) -> Self {
        Self {
            ok: true,
            command: command.name().to_string(),
            view: view.to_string(),
            elapsed_ms: started_at.elapsed().as_millis(),
            data,
            errors: Vec::new(),
        }
    }

    fn error(
        command: impl Into<String>,
        view: impl Into<String>,
        started_at: Instant,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            command: command.into(),
            view: view.into(),
            elapsed_ms: started_at.elapsed().as_millis(),
            data: Value::Null,
            errors: vec![AgentGuiError {
                code: code.into(),
                message: message.into(),
            }],
        }
    }
}

pub fn session_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SESSION_FILE_NAME)
}

pub fn read_session(config_dir: &Path) -> Result<AgentGuiSession, String> {
    let path = session_file_path(config_dir);
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

pub fn send_command_to_session(
    session: &AgentGuiSession,
    command: AgentGuiCommand,
) -> Result<AgentGuiResponse, String> {
    let mut stream = TcpStream::connect((session.host.as_str(), session.port)).map_err(|e| {
        format!(
            "Failed to connect to Foxy agent GUI driver at {}:{}: {}",
            session.host, session.port, e
        )
    })?;
    stream
        .set_read_timeout(Some(command.timeout()))
        .map_err(|e| format!("Failed to set read timeout: {}", e))?;
    stream
        .set_write_timeout(Some(DEFAULT_COMMAND_TIMEOUT))
        .map_err(|e| format!("Failed to set write timeout: {}", e))?;

    let request = AgentGuiWireRequest {
        token: session.token.clone(),
        command,
    };
    let line = serde_json::to_string(&request)
        .map_err(|e| format!("Failed to serialize agent GUI request: {}", e))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to write agent GUI request: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| format!("Failed to read agent GUI response: {}", e))?;
    if response.trim().is_empty() {
        return Err("Agent GUI driver returned an empty response".to_string());
    }
    serde_json::from_str(response.trim())
        .map_err(|e| format!("Failed to parse agent GUI response: {}", e))
}

pub fn start_service(
    config_dir: &Path,
    requested_port: u16,
    ctx: egui::Context,
) -> Result<AgentGuiRuntime, String> {
    let (tx, rx) = std::sync::mpsc::channel::<AgentGuiUiRequest>();
    let listener = TcpListener::bind(("127.0.0.1", requested_port))
        .map_err(|e| format!("Failed to bind agent GUI driver: {}", e))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read agent GUI driver address: {}", e))?;
    let token: String = rng()
        .sample_iter(&Alphanumeric)
        .take(40)
        .map(char::from)
        .collect();
    let session_file = session_file_path(config_dir);
    let session = AgentGuiSession {
        pid: std::process::id(),
        host: "127.0.0.1".to_string(),
        port: local_addr.port(),
        token: token.clone(),
        session_file,
    };

    if let Some(parent) = session.session_file.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    let serialized = serde_json::to_string_pretty(&session)
        .map_err(|e| format!("Failed to serialize agent GUI session: {}", e))?;
    fs::write(&session.session_file, serialized)
        .map_err(|e| format!("Failed to write {}: {}", session.session_file.display(), e))?;

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(err) => {
                    log::warn!("Agent GUI driver connection failed: {}", err);
                    continue;
                }
            };
            let tx = tx.clone();
            let token = token.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || handle_connection(stream, tx, &token, &ctx));
        }
    });

    log::info!(
        "Agent GUI driver listening on 127.0.0.1:{}; session file {}",
        session.port,
        session.session_file.display()
    );
    Ok(AgentGuiRuntime::new(rx, session))
}

fn handle_connection(
    mut stream: TcpStream,
    tx: Sender<AgentGuiUiRequest>,
    token: &str,
    ctx: &egui::Context,
) {
    // A connection may carry one request (the classic one-shot client) or a
    // newline-delimited stream of them (the persistent `exec --stdin` client).
    // Loop until EOF so the socket + token handshake are amortized across a
    // whole interactive session.
    let _ = stream.set_write_timeout(Some(DEFAULT_COMMAND_TIMEOUT));
    let read_stream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(err) => {
            let response = AgentGuiResponse::error(
                "unknown",
                "",
                Instant::now(),
                "io",
                format!("Failed to clone stream: {err}"),
            );
            let _ = write_response(&mut stream, &response);
            return;
        }
    };
    let mut reader = BufReader::new(read_stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return, // clean EOF
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(err) => {
                let response = AgentGuiResponse::error(
                    "unknown",
                    "",
                    Instant::now(),
                    "io",
                    format!("Read failed: {err}"),
                );
                let _ = write_response(&mut stream, &response);
                return;
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = dispatch_wire_line(line.trim(), &tx, token, ctx);
        if write_response(&mut stream, &response).is_err() {
            return;
        }
    }
}

/// Parse, authenticate, and run one wire request line, returning the response
/// (used by both the one-shot and persistent connection paths).
fn dispatch_wire_line(
    line: &str,
    tx: &Sender<AgentGuiUiRequest>,
    token: &str,
    ctx: &egui::Context,
) -> AgentGuiResponse {
    let started_at = Instant::now();
    let request: AgentGuiWireRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(err) => {
            return AgentGuiResponse::error(
                "unknown",
                "",
                started_at,
                "invalid-json",
                format!("Invalid request JSON: {err}"),
            );
        }
    };
    if request.token != token {
        return AgentGuiResponse::error("unknown", "", started_at, "unauthorized", "Invalid token");
    }

    let timeout = request.command.timeout();
    let (response_tx, response_rx) = std::sync::mpsc::channel();
    let ui_request = AgentGuiUiRequest {
        request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        command: request.command,
        response_tx,
    };
    if tx.send(ui_request).is_err() {
        return AgentGuiResponse::error(
            "unknown",
            "",
            started_at,
            "disconnected",
            "Foxy UI is not accepting agent GUI commands",
        );
    }
    ctx.request_repaint();

    match response_rx.recv_timeout(timeout) {
        Ok(response) => response,
        Err(_) => AgentGuiResponse::error(
            "unknown",
            "",
            started_at,
            "timeout",
            "Timed out waiting for Foxy UI response",
        ),
    }
}

fn write_response(stream: &mut TcpStream, response: &AgentGuiResponse) -> std::io::Result<()> {
    let line = serde_json::to_string(response).unwrap_or_else(|_| {
        "{\"ok\":false,\"command\":\"serialization\",\"view\":\"\",\"elapsed_ms\":0,\"data\":null,\"errors\":[{\"code\":\"serialization\",\"message\":\"Failed to serialize response\"}]}".to_string()
    });
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")
}

impl Foxy {
    pub(crate) fn initialize_agent_gui(
        &mut self,
        ctx: &egui::Context,
        launch_config: &AgentGuiLaunchConfig,
        active_renderer: &'static str,
    ) {
        if !launch_config.enabled {
            return;
        }
        match start_service(
            &Self::get_config_directory(),
            launch_config.port,
            ctx.clone(),
        ) {
            Ok(mut runtime) => {
                runtime.active_renderer = Some(active_renderer);
                self.agent_gui = Some(runtime);
                // Gate dialog interception + virtual-clock reads on the driver
                // actually running, so a normal user session is never affected.
                crate::ui::app::agent_support::set_agent_gui_active(true);
            }
            Err(err) => {
                log::error!("Failed to start agent GUI driver: {}", err);
                eprintln!("FATAL: Failed to start agent GUI driver: {err}");
                std::process::exit(1);
            }
        }
    }

    pub(crate) fn poll_agent_gui(&mut self, ctx: &egui::Context) {
        let Some(mut runtime) = self.agent_gui.take() else {
            return;
        };

        // Re-assert stable-render each frame so frozen animation time / disabled
        // blink survive egui's per-frame style reset while the mode is on.
        if runtime.stable_render {
            self.apply_agent_gui_stable_render(ctx, true);
        }

        self.agent_gui_complete_screenshots(ctx, &mut runtime);

        while let Ok(request) = runtime.rx.try_recv() {
            self.handle_agent_gui_request(ctx, &mut runtime, request);
        }

        self.agent_gui_complete_waits(ctx, &mut runtime);
        self.agent_gui_complete_settles(ctx, &mut runtime);
        self.agent_gui_complete_drags(ctx, &mut runtime);
        // Resume any parked batch pipelines after the per-step completion passes
        // above so a step that finished this frame is picked up promptly.
        self.agent_gui_advance_batches(ctx, &mut runtime);
        // Derive view/modal/focus/toast/download transition events from the
        // frame we just observed (the causal feed behind `events`).
        self.agent_gui_record_state_events(ctx, &mut runtime);
        if !runtime.pending_waits.is_empty()
            || !runtime.pending_screenshots.is_empty()
            || !runtime.pending_settles.is_empty()
            || !runtime.pending_drags.is_empty()
            || !runtime.pending_batches.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        self.agent_gui = Some(runtime);
    }

    fn handle_agent_gui_request(
        &mut self,
        ctx: &egui::Context,
        runtime: &mut AgentGuiRuntime,
        request: AgentGuiUiRequest,
    ) {
        let started_at = Instant::now();
        let view = self.agent_gui_view_name().to_string();
        let command = request.command.clone();
        let response = match &command {
            AgentGuiCommand::Status => {
                let data = json!({
                    "pid": runtime.session.pid,
                    "host": runtime.session.host,
                    "port": runtime.session.port,
                    "session_file": runtime.session.session_file,
                    "uptime_ms": runtime.started_at.elapsed().as_millis(),
                    "debug_mode": self.settings_view_state.debug_mode,
                    "startup_frame_rendered": self.startup_frame_rendered,
                    "busy": self.agent_gui_busy(),
                    "active_modal_count": self.agent_gui_active_modal_count(),
                });
                AgentGuiResponse::ok(&command, &view, started_at, data)
            }
            AgentGuiCommand::OpenView {
                view,
                repository_index,
                tab,
            } => match self.agent_gui_open_view(view, *repository_index, tab.as_deref()) {
                Ok(()) => {
                    ctx.request_repaint();
                    AgentGuiResponse::ok(
                        &command,
                        self.agent_gui_view_name(),
                        started_at,
                        self.agent_gui_snapshot_value(ctx),
                    )
                }
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    self.agent_gui_view_name(),
                    started_at,
                    "invalid-view",
                    message,
                ),
            },
            AgentGuiCommand::Snapshot {
                fields,
                since_frame,
            } => {
                let current_frame = ctx.cumulative_frame_nr();
                if let Some(since) = since_frame
                    && current_frame <= *since
                {
                    // Nothing rendered since the caller's cursor; skip re-paying
                    // for the full snapshot. The cursor is real per-frame state.
                    AgentGuiResponse::ok(
                        &command,
                        &view,
                        started_at,
                        json!({ "changed": false, "frame": current_frame }),
                    )
                } else {
                    // Build once: store the full observation as a future `diff`
                    // baseline, then serialize/project for the response.
                    let snapshot = self.agent_gui_snapshot(ctx);
                    runtime.push_diff_baseline(snapshot.clone());
                    let mut value = serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({}));
                    if let Some(fields) = fields {
                        value = project_object_fields(value, fields);
                    }
                    AgentGuiResponse::ok(&command, &view, started_at, value)
                }
            }
            AgentGuiCommand::Text { contains, limit } => {
                let mut texts = self.agent_gui_snapshot(ctx).texts;
                if let Some(contains) = contains {
                    let needle = contains.to_ascii_lowercase();
                    texts.retain(|text| text.to_ascii_lowercase().contains(&needle));
                }
                if let Some(limit) = limit {
                    texts.truncate(*limit);
                }
                AgentGuiResponse::ok(&command, &view, started_at, json!({ "texts": texts }))
            }
            AgentGuiCommand::Find {
                text,
                role,
                id,
                visible_only,
            } => {
                let nodes = self.agent_gui_find_nodes(
                    ctx,
                    text.as_deref(),
                    role.as_deref(),
                    id.as_deref(),
                    *visible_only,
                );
                AgentGuiResponse::ok(&command, &view, started_at, json!({ "nodes": nodes }))
            }
            AgentGuiCommand::Click {
                text,
                id,
                x,
                y,
                modifiers,
                button,
                double,
            } => match self.agent_gui_click(
                ctx,
                text.as_deref(),
                id.as_deref(),
                *x,
                *y,
                *modifiers,
                button.as_deref(),
                *double,
            ) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => {
                    AgentGuiResponse::error(command.name(), &view, started_at, "not-found", message)
                }
            },
            AgentGuiCommand::Scroll {
                id,
                x,
                y,
                dx,
                dy,
                modifiers,
            } => {
                let pos = self.agent_gui_pointer_pos(ctx, id.as_deref(), *x, *y);
                let mods = modifiers.to_egui();
                ctx.input_mut(|input| {
                    input.events.push(Event::PointerMoved(pos));
                    input.events.push(Event::MouseWheel {
                        unit: MouseWheelUnit::Point,
                        delta: Vec2::new(*dx, *dy),
                        phase: TouchPhase::Move,
                        modifiers: mods,
                    });
                });
                ctx.request_repaint();
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "queued": true, "x": pos.x, "y": pos.y, "dx": dx, "dy": dy }),
                )
            }
            AgentGuiCommand::Hover { id, x, y } => {
                let pos = self.agent_gui_pointer_pos(ctx, id.as_deref(), *x, *y);
                ctx.input_mut(|input| input.events.push(Event::PointerMoved(pos)));
                ctx.request_repaint();
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "queued": true, "x": pos.x, "y": pos.y }),
                )
            }
            AgentGuiCommand::MouseDown {
                id,
                x,
                y,
                modifiers,
                button,
            } => match self.agent_gui_mouse_button(
                ctx,
                id.as_deref(),
                *x,
                *y,
                *modifiers,
                button.as_deref(),
                true,
            ) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-button",
                    message,
                ),
            },
            AgentGuiCommand::MouseUp {
                id,
                x,
                y,
                modifiers,
                button,
            } => match self.agent_gui_mouse_button(
                ctx,
                id.as_deref(),
                *x,
                *y,
                *modifiers,
                button.as_deref(),
                false,
            ) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-button",
                    message,
                ),
            },
            AgentGuiCommand::Key { key, modifiers } => match parse_agent_gui_key(key) {
                Some(parsed) => {
                    let mods = modifiers.to_egui();
                    ctx.input_mut(|input| {
                        input.events.push(Event::Key {
                            key: parsed,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: mods,
                        });
                        input.events.push(Event::Key {
                            key: parsed,
                            physical_key: None,
                            pressed: false,
                            repeat: false,
                            modifiers: mods,
                        });
                    });
                    ctx.request_repaint();
                    AgentGuiResponse::ok(&command, &view, started_at, json!({ "queued": true }))
                }
                None => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-key",
                    format!("Unsupported key '{}'", key),
                ),
            },
            AgentGuiCommand::Type { text } => {
                ctx.input_mut(|input| input.events.push(Event::Text(text.clone())));
                ctx.request_repaint();
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "queued": true, "chars": text.chars().count() }),
                )
            }
            AgentGuiCommand::Screenshot { output, annotate } => {
                let output = output.clone();
                // Capture the node overlay map now so the sidecar reflects the
                // frame the screenshot was requested from.
                let annotation = annotate.then(|| self.agent_gui_nodes(ctx));
                runtime.pending_screenshots.push(PendingScreenshot {
                    request_id: request.request_id,
                    output,
                    response_tx: request.response_tx,
                    requested_at: started_at,
                    annotation,
                });
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::new(
                    request.request_id,
                )));
                ctx.request_repaint();
                return;
            }
            AgentGuiCommand::Fps => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                json!({
                    "fps": self.fps_ema,
                    "fps_counter_visible": self.settings_view_state.show_fps_counter,
                    // Diff these across two reads to detect multi-pass: if
                    // (pass_delta - frame_delta) > 0 between two scroll samples,
                    // egui is running extra layout passes (the cost behind the
                    // "changed id between passes" warning / scroll FPS drop).
                    "cumulative_frame_nr": ctx.cumulative_frame_nr(),
                    "cumulative_pass_nr": ctx.cumulative_pass_nr(),
                }),
            ),
            AgentGuiCommand::Wait {
                condition,
                timeout_ms,
            } => {
                if self.agent_gui_wait_satisfied(ctx, condition) {
                    AgentGuiResponse::ok(
                        &command,
                        &view,
                        started_at,
                        self.agent_gui_snapshot_value(ctx),
                    )
                } else {
                    runtime.pending_waits.push(PendingWait {
                        request_id: request.request_id,
                        command: command.clone(),
                        condition: condition.clone(),
                        response_tx: request.response_tx,
                        deadline: Instant::now()
                            .checked_add(Duration::from_millis(*timeout_ms))
                            .unwrap_or_else(Instant::now),
                        requested_at: started_at,
                    });
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
            }
            AgentGuiCommand::Logs {
                level,
                contains,
                limit,
                since_generation,
            } => {
                let data = agent_gui_logs(
                    level.as_deref(),
                    contains.as_deref(),
                    *limit,
                    *since_generation,
                );
                AgentGuiResponse::ok(&command, &view, started_at, data)
            }
            AgentGuiCommand::Repositories { contains, limit } => {
                let data = self.agent_gui_repositories_value(contains.as_deref(), *limit);
                AgentGuiResponse::ok(&command, &view, started_at, data)
            }
            AgentGuiCommand::Addons {
                repository_index,
                tab,
                contains,
                enabled_only,
                limit,
            } => match self.agent_gui_addons_value(
                *repository_index,
                tab.as_deref(),
                contains.as_deref(),
                *enabled_only,
                *limit,
            ) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => {
                    AgentGuiResponse::error(command.name(), &view, started_at, "not-found", message)
                }
            },
            AgentGuiCommand::Settings => {
                AgentGuiResponse::ok(&command, &view, started_at, self.agent_gui_settings_value())
            }
            AgentGuiCommand::Progress => {
                AgentGuiResponse::ok(&command, &view, started_at, self.agent_gui_progress_value())
            }
            AgentGuiCommand::Scale { percent } => {
                let applied = self.agent_gui_set_scale(ctx, *percent);
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "ui_scale_percent": applied }),
                )
            }
            AgentGuiCommand::Resize { width, height } => {
                let size = Vec2::new(width.max(1.0), height.max(1.0));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                ctx.request_repaint();
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "queued": true, "width": size.x, "height": size.y }),
                )
            }
            AgentGuiCommand::Profiles {
                repository_index,
                contains,
                limit,
            } => {
                match self.agent_gui_profiles_value(*repository_index, contains.as_deref(), *limit)
                {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "not-found",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Missions { contains, limit } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_missions_value(contains.as_deref(), *limit),
            ),
            AgentGuiCommand::Spaces { contains, limit } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_spaces_value(contains.as_deref(), *limit),
            ),
            AgentGuiCommand::DownloadSummary { include_telemetry } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_download_summary_value(*include_telemetry),
            ),
            AgentGuiCommand::Toasts => {
                AgentGuiResponse::ok(&command, &view, started_at, self.agent_gui_toasts_value())
            }
            AgentGuiCommand::SetSetting { key, value } => {
                match self.agent_gui_set_setting(ctx, key, value) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "invalid-setting",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Health => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_health_value(runtime),
            ),
            AgentGuiCommand::Focus { target, clear } => {
                match self.agent_gui_focus(ctx, target.as_deref(), *clear) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "not-found",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Nav { count, reverse } => {
                let mods = AgentGuiModifiers {
                    shift: *reverse,
                    ..Default::default()
                }
                .to_egui();
                ctx.input_mut(|input| {
                    for _ in 0..(*count).max(1) {
                        input.events.push(Event::Key {
                            key: Key::Tab,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: mods,
                        });
                        input.events.push(Event::Key {
                            key: Key::Tab,
                            physical_key: None,
                            pressed: false,
                            repeat: false,
                            modifiers: mods,
                        });
                    }
                });
                ctx.request_repaint();
                // Park one frame so egui consumes the Tab events, then report
                // where focus landed.
                runtime.pending_settles.push(PendingSettle {
                    command: command.clone(),
                    target_frame: ctx.cumulative_frame_nr().saturating_add(1),
                    response_tx: request.response_tx,
                    deadline: Instant::now()
                        .checked_add(DEFAULT_COMMAND_TIMEOUT)
                        .unwrap_or_else(Instant::now),
                    requested_at: started_at,
                    report_focus: true,
                });
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            AgentGuiCommand::Fill { target, value } => {
                match self.agent_gui_fill(ctx, target, value) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "not-found",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Filters => {
                AgentGuiResponse::ok(&command, &view, started_at, self.agent_gui_filters_value())
            }
            AgentGuiCommand::SetFilter { name, value } => {
                match self.agent_gui_set_filter(ctx, name, value) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "invalid-filter",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Select {
                repository,
                server,
                mission,
                space,
            } => match self.agent_gui_select(ctx, *repository, *server, *mission, space.as_deref())
            {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => {
                    AgentGuiResponse::error(command.name(), &view, started_at, "not-found", message)
                }
            },
            AgentGuiCommand::Window { action } => match self.agent_gui_window(ctx, action) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-action",
                    message,
                ),
            },
            AgentGuiCommand::Settle { frames } => {
                runtime.pending_settles.push(PendingSettle {
                    command: command.clone(),
                    target_frame: ctx.cumulative_frame_nr().saturating_add((*frames).max(1)),
                    response_tx: request.response_tx,
                    deadline: Instant::now()
                        .checked_add(command.timeout())
                        .unwrap_or_else(Instant::now),
                    requested_at: started_at,
                    report_focus: false,
                });
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            AgentGuiCommand::StableRender { on } => {
                runtime.stable_render = *on;
                self.apply_agent_gui_stable_render(ctx, *on);
                ctx.request_repaint();
                AgentGuiResponse::ok(&command, &view, started_at, json!({ "stable_render": on }))
            }
            AgentGuiCommand::Assert {
                field,
                equals,
                contains,
                repository_index,
            } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_assert_value(
                    ctx,
                    field,
                    equals.as_deref(),
                    contains.as_deref(),
                    *repository_index,
                ),
            ),
            AgentGuiCommand::ContextMenu { id, x, y } => {
                // egui context-menu popups are not introspectable, so this is a
                // secondary-click helper that opens the menu; read the resulting
                // text with a follow-up snapshot/text call.
                match self.agent_gui_mouse_button(
                    ctx,
                    id.as_deref(),
                    *x,
                    *y,
                    AgentGuiModifiers::default(),
                    Some("secondary"),
                    true,
                ) {
                    Ok(down) => {
                        let _ = self.agent_gui_mouse_button(
                            ctx,
                            id.as_deref(),
                            down.get("x").and_then(Value::as_f64).map(|v| v as f32),
                            down.get("y").and_then(Value::as_f64).map(|v| v as f32),
                            AgentGuiModifiers::default(),
                            Some("secondary"),
                            false,
                        );
                        AgentGuiResponse::ok(
                            &command,
                            &view,
                            started_at,
                            json!({ "queued": true, "x": down.get("x"), "y": down.get("y") }),
                        )
                    }
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "invalid-target",
                        message,
                    ),
                }
            }
            AgentGuiCommand::MenuSelect { item } => {
                // Popup entries are plain text, so reuse the text/semantic click
                // path to activate one by its visible label.
                match self.agent_gui_click(
                    ctx,
                    Some(item),
                    None,
                    None,
                    None,
                    AgentGuiModifiers::default(),
                    None,
                    false,
                ) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "not-found",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Inventory {
                contains,
                folder,
                source,
                limit,
            } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_inventory_value(
                    contains.as_deref(),
                    folder.as_deref(),
                    source.as_deref(),
                    *limit,
                ),
            ),
            AgentGuiCommand::PendingUpdates {
                repository_index,
                contains,
                limit,
                include_files,
            } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_pending_updates_value(
                    *repository_index,
                    contains.as_deref(),
                    *limit,
                    *include_files,
                ),
            ),
            AgentGuiCommand::AppUpdate => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_app_update_value(),
            ),
            AgentGuiCommand::Memory { history, textures } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_memory_value(*history, *textures),
            ),
            AgentGuiCommand::ArmaProfiles => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_arma_profiles_value(),
            ),
            AgentGuiCommand::Backups { contains, limit } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                self.agent_gui_backups_value(contains.as_deref(), *limit),
            ),
            AgentGuiCommand::Invoke {
                action,
                params,
                allow_destructive,
                list_actions,
            } => {
                if *list_actions || action.is_none() {
                    AgentGuiResponse::ok(
                        &command,
                        &view,
                        started_at,
                        agent_gui_list_actions_value(),
                    )
                } else {
                    let action = action.as_deref().unwrap_or_default();
                    match self.agent_gui_invoke(ctx, action, params, *allow_destructive) {
                        Ok(mut data) => {
                            runtime.record_event("invoke", json!({ "action": action, "ok": true }));
                            if let Value::Object(map) = &mut data {
                                map.insert(
                                    "snapshot".to_string(),
                                    self.agent_gui_snapshot_value(ctx),
                                );
                            }
                            AgentGuiResponse::ok(
                                &command,
                                self.agent_gui_view_name(),
                                started_at,
                                data,
                            )
                        }
                        Err((code, message)) => AgentGuiResponse::error(
                            command.name(),
                            &view,
                            started_at,
                            code,
                            message,
                        ),
                    }
                }
            }
            AgentGuiCommand::Batch {
                steps,
                stop_on_error,
            } => {
                self.agent_gui_start_batch(
                    ctx,
                    runtime,
                    steps.clone(),
                    *stop_on_error,
                    request.response_tx,
                    started_at,
                );
                return;
            }
            AgentGuiCommand::Diff { baseline } => {
                let (data, current) = self.agent_gui_diff_value(ctx, runtime, baseline);
                // The current observation becomes available as a future
                // `frame:<n>` / `last` baseline.
                runtime.push_diff_baseline(current);
                AgentGuiResponse::ok(&command, &view, started_at, data)
            }
            AgentGuiCommand::Drag {
                from_id,
                from_x,
                from_y,
                to_id,
                to_x,
                to_y,
                steps,
                button,
            } => {
                let from = self.agent_gui_pointer_pos(ctx, from_id.as_deref(), *from_x, *from_y);
                let to = self.agent_gui_pointer_pos(ctx, to_id.as_deref(), *to_x, *to_y);
                match parse_pointer_button(button.as_deref()) {
                    Ok(button) => {
                        runtime.record_event(
                            "drag",
                            json!({ "from": { "x": from.x, "y": from.y }, "to": { "x": to.x, "y": to.y } }),
                        );
                        runtime.pending_drags.push(PendingDrag {
                            command: command.clone(),
                            from,
                            to,
                            button,
                            steps: (*steps).max(1),
                            phase: 0,
                            next_frame: ctx.cumulative_frame_nr(),
                            response_tx: request.response_tx,
                            deadline: Instant::now()
                                .checked_add(command.timeout())
                                .unwrap_or_else(Instant::now),
                            requested_at: started_at,
                        });
                        ctx.request_repaint();
                        return;
                    }
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "invalid-button",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Query { expr } => match self.agent_gui_query_value(ctx, expr) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-query",
                    message,
                ),
            },
            AgentGuiCommand::Checkpoint { name, list } => {
                if *list || name.is_none() {
                    let names: Vec<&String> = runtime.checkpoints.keys().collect();
                    AgentGuiResponse::ok(
                        &command,
                        &view,
                        started_at,
                        json!({ "checkpoints": names }),
                    )
                } else {
                    let name = name.clone().unwrap_or_default();
                    let state = self.agent_gui_capture_checkpoint(ctx);
                    runtime.checkpoints.insert(name.clone(), state);
                    AgentGuiResponse::ok(
                        &command,
                        &view,
                        started_at,
                        json!({ "checkpoint": name, "saved": true, "ui_state_only": true }),
                    )
                }
            }
            AgentGuiCommand::Restore { name } => match runtime.checkpoints.get(name).cloned() {
                Some(state) => {
                    self.agent_gui_restore_checkpoint(ctx, &state);
                    let mut data =
                        json!({ "checkpoint": name, "restored": true, "ui_state_only": true });
                    if let Value::Object(map) = &mut data {
                        map.insert("snapshot".to_string(), self.agent_gui_snapshot_value(ctx));
                    }
                    AgentGuiResponse::ok(&command, self.agent_gui_view_name(), started_at, data)
                }
                None => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "not-found",
                    format!("No checkpoint named '{name}'"),
                ),
            },
            AgentGuiCommand::Element { id, x, y } => {
                match self.agent_gui_element_value(ctx, id.as_deref(), *x, *y) {
                    Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                    Err(message) => AgentGuiResponse::error(
                        command.name(),
                        &view,
                        started_at,
                        "not-found",
                        message,
                    ),
                }
            }
            AgentGuiCommand::Events {
                kinds,
                since,
                limit,
            } => AgentGuiResponse::ok(
                &command,
                &view,
                started_at,
                agent_gui_events_value(runtime, kinds.as_deref(), *since, *limit),
            ),
            AgentGuiCommand::Clock { action, ms } => match agent_gui_clock(ctx, action, *ms) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-action",
                    message,
                ),
            },
            AgentGuiCommand::Dialog {
                action,
                path,
                cancel,
            } => match agent_gui_dialog(action, path.as_deref(), *cancel) {
                Ok(data) => AgentGuiResponse::ok(&command, &view, started_at, data),
                Err(message) => AgentGuiResponse::error(
                    command.name(),
                    &view,
                    started_at,
                    "invalid-action",
                    message,
                ),
            },
            AgentGuiCommand::Close => {
                self.request_app_close(ctx, "agent GUI driver");
                AgentGuiResponse::ok(
                    &command,
                    &view,
                    started_at,
                    json!({ "close_requested": true }),
                )
            }
        };

        // Record injected-input events for the causal `events` feed (state
        // transitions are derived separately in `poll`). Only on success.
        if response.ok {
            match &command {
                AgentGuiCommand::Click { .. } => {
                    runtime.record_event("click", response.data.clone())
                }
                AgentGuiCommand::Key { key, .. } => {
                    runtime.record_event("key", json!({ "key": key }))
                }
                AgentGuiCommand::Type { text } => {
                    runtime.record_event("type", json!({ "chars": text.chars().count() }))
                }
                AgentGuiCommand::Scroll { .. } => {
                    runtime.record_event("scroll", response.data.clone())
                }
                _ => {}
            }
        }

        let _ = request.response_tx.send(response);
    }

    fn agent_gui_complete_screenshots(
        &mut self,
        ctx: &egui::Context,
        runtime: &mut AgentGuiRuntime,
    ) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let Event::Screenshot {
                user_data, image, ..
            } = event
            else {
                continue;
            };
            let Some(request_id) = user_data
                .data
                .as_ref()
                .and_then(|data| data.downcast_ref::<u64>())
                .copied()
            else {
                continue;
            };
            let Some(index) = runtime
                .pending_screenshots
                .iter()
                .position(|pending| pending.request_id == request_id)
            else {
                continue;
            };
            let pending = runtime.pending_screenshots.remove(index);
            let ppp = ctx.pixels_per_point();
            let pointer = ctx.pointer_latest_pos();
            let write_result = match &pending.annotation {
                Some(nodes) => {
                    let mut annotated = (*image).clone();
                    annotate_color_image(&mut annotated, nodes, pointer, ppp);
                    write_color_image_png(&pending.output, &annotated).and_then(|()| {
                        write_annotation_sidecar(&pending.output, nodes, pointer, ppp)
                    })
                }
                None => write_color_image_png(&pending.output, &image),
            };
            let response = match write_result {
                Ok(()) => {
                    let mut data = json!({
                        "screenshot_path": pending.output,
                        "width": image.size[0],
                        "height": image.size[1],
                        "scale_factor": ppp,
                    });
                    if let (Some(nodes), Value::Object(map)) = (&pending.annotation, &mut data) {
                        map.insert("annotated".to_string(), json!(true));
                        map.insert("node_count".to_string(), json!(nodes.len()));
                        map.insert(
                            "sidecar_path".to_string(),
                            json!(annotation_sidecar_path(&pending.output)),
                        );
                    }
                    AgentGuiResponse::ok(
                        &AgentGuiCommand::Screenshot {
                            output: pending.output.clone(),
                            annotate: pending.annotation.is_some(),
                        },
                        self.agent_gui_view_name(),
                        pending.requested_at,
                        data,
                    )
                }
                Err(message) => AgentGuiResponse::error(
                    "screenshot",
                    self.agent_gui_view_name(),
                    pending.requested_at,
                    "write-failed",
                    message,
                ),
            };
            let _ = pending.response_tx.send(response);
        }

        let now = Instant::now();
        let mut index = 0;
        while index < runtime.pending_screenshots.len() {
            if now.duration_since(runtime.pending_screenshots[index].requested_at)
                <= SCREENSHOT_TIMEOUT
            {
                index += 1;
                continue;
            }
            let pending = runtime.pending_screenshots.remove(index);
            let response = AgentGuiResponse::error(
                "screenshot",
                self.agent_gui_view_name(),
                pending.requested_at,
                "timeout",
                "Timed out waiting for egui screenshot event",
            );
            let _ = pending.response_tx.send(response);
        }
    }

    fn agent_gui_complete_waits(&mut self, ctx: &egui::Context, runtime: &mut AgentGuiRuntime) {
        let now = Instant::now();
        let mut index = 0;
        while index < runtime.pending_waits.len() {
            let satisfied =
                self.agent_gui_wait_satisfied(ctx, &runtime.pending_waits[index].condition);
            if satisfied {
                let pending = runtime.pending_waits.remove(index);
                let response = AgentGuiResponse::ok(
                    &pending.command,
                    self.agent_gui_view_name(),
                    pending.requested_at,
                    self.agent_gui_snapshot_value(ctx),
                );
                let _ = pending.response_tx.send(response);
                continue;
            }
            if now >= runtime.pending_waits[index].deadline {
                let pending = runtime.pending_waits.remove(index);
                let response = AgentGuiResponse::error(
                    pending.command.name(),
                    self.agent_gui_view_name(),
                    pending.requested_at,
                    "timeout",
                    format!("Timed out waiting for request {}", pending.request_id),
                );
                let _ = pending.response_tx.send(response);
                continue;
            }
            index += 1;
        }
    }

    /// Resolve parked `settle`/`nav` responses once the frame clock reaches
    /// their target frame, returning the post-input snapshot (plus the resolved
    /// focus for `nav`).
    fn agent_gui_complete_settles(&mut self, ctx: &egui::Context, runtime: &mut AgentGuiRuntime) {
        let now = Instant::now();
        let current_frame = ctx.cumulative_frame_nr();
        let mut index = 0;
        while index < runtime.pending_settles.len() {
            if current_frame >= runtime.pending_settles[index].target_frame {
                let pending = runtime.pending_settles.remove(index);
                let mut data = self.agent_gui_snapshot_value(ctx);
                if pending.report_focus
                    && let Value::Object(map) = &mut data
                {
                    map.insert("focused".to_string(), json!(agent_gui_focused_name(ctx)));
                }
                let response = AgentGuiResponse::ok(
                    &pending.command,
                    self.agent_gui_view_name(),
                    pending.requested_at,
                    data,
                );
                let _ = pending.response_tx.send(response);
                continue;
            }
            if now >= runtime.pending_settles[index].deadline {
                let pending = runtime.pending_settles.remove(index);
                // A settle that times out still returns the current snapshot:
                // the frame simply did not advance, which is itself observable.
                let response = AgentGuiResponse::ok(
                    &pending.command,
                    self.agent_gui_view_name(),
                    pending.requested_at,
                    self.agent_gui_snapshot_value(ctx),
                );
                let _ = pending.response_tx.send(response);
                continue;
            }
            index += 1;
        }
    }

    fn agent_gui_open_view(
        &mut self,
        view: &str,
        repository_index: Option<usize>,
        tab: Option<&str>,
    ) -> Result<(), String> {
        let parsed =
            parse_agent_gui_view(view).ok_or_else(|| format!("Unsupported view '{}'", view))?;
        self.update_modal_open = false;
        match parsed {
            FoxyView::RepositorySettings => {
                let repo_index = repository_index
                    .or(self.repository_view_state.selected_repository)
                    .ok_or_else(|| {
                        "Repository settings requires --repo-index or a selected repository"
                            .to_string()
                    })?;
                if repo_index >= self.repository_view_state.repositories.len() {
                    return Err(format!("Repository index {} is out of range", repo_index));
                }
                if let Some(tab) = tab {
                    self.current_repository_settings_tab =
                        parse_agent_gui_repository_settings_tab(tab).ok_or_else(|| {
                            format!("Unsupported repository-settings tab '{}'", tab)
                        })?;
                }
                self.selected_repository_for_settings = Some(repo_index);
                self.repository_view_state.selected_repository = Some(repo_index);
                self.current_view = FoxyView::RepositorySettings;
            }
            FoxyView::RepositorySpaceSettings => {
                self.current_view = FoxyView::RepositorySpaceSettings;
            }
            FoxyView::Settings => {
                if self.current_view != FoxyView::Settings {
                    self.open_settings_view();
                }
                if let Some(tab) = tab {
                    self.settings_view_state.current_tab = parse_agent_gui_settings_tab(tab)
                        .ok_or_else(|| format!("Unsupported settings tab '{}'", tab))?;
                }
            }
            FoxyView::Help
            | FoxyView::Changelog
            | FoxyView::About
            | FoxyView::AppUpdate
            | FoxyView::VersionBrowser => {
                self.open_reference_view(parsed);
            }
            FoxyView::GameSpaces => {
                if self.current_view != FoxyView::GameSpaces {
                    self.open_game_spaces_view();
                }
            }
            FoxyView::GameSpaceSettings => {
                if self.current_view != FoxyView::GameSpaceSettings {
                    self.open_active_game_space_settings();
                }
                if let Some(tab) = tab {
                    self.game_space_settings_view_state.current_tab =
                        parse_agent_gui_game_space_settings_tab(tab).ok_or_else(|| {
                            format!("Unsupported game-space-settings tab '{}'", tab)
                        })?;
                }
            }
            FoxyView::RepositoryList | FoxyView::SwiftyMigration | FoxyView::None => {
                self.current_view = parsed;
                self.last_view = FoxyView::None;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_gui_click(
        &mut self,
        ctx: &egui::Context,
        text: Option<&str>,
        id: Option<&str>,
        x: Option<f32>,
        y: Option<f32>,
        modifiers: AgentGuiModifiers,
        button: Option<&str>,
        double: bool,
    ) -> Result<Value, String> {
        if let Some(id) = id
            && self.agent_gui_click_semantic(ctx, id)
        {
            return Ok(json!({ "semantic": true, "id": id }));
        }
        if let Some(text) = text
            && self.agent_gui_click_semantic(ctx, text)
        {
            return Ok(json!({ "semantic": true, "text": text }));
        }

        let pos = if let (Some(x), Some(y)) = (x, y) {
            Pos2::new(x, y)
        } else if let Some(id) = id {
            self.agent_gui_node_center(ctx, id)
                .ok_or_else(|| format!("No semantic target or known node matched id '{id}'"))?
        } else {
            return Err("Provide --text, --id, or both --x and --y".to_string());
        };

        let button = parse_pointer_button(button)?;
        let mods = modifiers.to_egui();
        let clicks = if double { 2 } else { 1 };
        ctx.input_mut(|input| {
            input.events.push(Event::PointerMoved(pos));
            for _ in 0..clicks {
                input.events.push(Event::PointerButton {
                    pos,
                    button,
                    pressed: true,
                    modifiers: mods,
                });
                input.events.push(Event::PointerButton {
                    pos,
                    button,
                    pressed: false,
                    modifiers: mods,
                });
            }
        });
        ctx.request_repaint();
        Ok(json!({ "queued": true, "x": pos.x, "y": pos.y, "double": double }))
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_gui_mouse_button(
        &self,
        ctx: &egui::Context,
        id: Option<&str>,
        x: Option<f32>,
        y: Option<f32>,
        modifiers: AgentGuiModifiers,
        button: Option<&str>,
        pressed: bool,
    ) -> Result<Value, String> {
        let button = parse_pointer_button(button)?;
        let pos = self.agent_gui_pointer_pos(ctx, id, x, y);
        let mods = modifiers.to_egui();
        ctx.input_mut(|input| {
            input.events.push(Event::PointerMoved(pos));
            input.events.push(Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: mods,
            });
        });
        ctx.request_repaint();
        Ok(json!({ "queued": true, "x": pos.x, "y": pos.y, "pressed": pressed }))
    }

    /// Center of a known semantic node by id, if it has a rect.
    fn agent_gui_node_center(&self, ctx: &egui::Context, id: &str) -> Option<Pos2> {
        let normalized = normalize_selector(id);
        self.agent_gui_nodes(ctx)
            .into_iter()
            .find(|node| normalize_selector(&node.id) == normalized)
            .and_then(|node| node.rect)
            .map(|rect| Pos2::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0))
    }

    fn agent_gui_click_semantic(&mut self, ctx: &egui::Context, selector: &str) -> bool {
        let selector = selector.trim();
        let normalized = normalize_selector(selector);
        match normalized.as_str() {
            "footer.changelog" | "open changelog" | "changelog" => {
                self.open_reference_view(FoxyView::Changelog);
            }
            "header.settings" | "settings" => {
                self.open_settings_view();
            }
            "footer.about" | "open about" | "about" => {
                self.open_reference_view(FoxyView::About);
            }
            "footer.help" | "open help" | "help" => {
                self.current_help_tab = crate::ui::types::HelpTab::Overview;
                self.open_reference_view(FoxyView::Help);
            }
            "footer.activity-log" | "show activity log" | "activity log" => {
                self.set_activity_log_visibility(
                    ctx,
                    !self.settings_view_state.show_activity_log,
                    "agent GUI driver",
                );
            }
            "app.close" | "close" => {
                self.request_app_close(ctx, "agent GUI driver semantic click");
            }
            _ => return false,
        }
        ctx.request_repaint();
        true
    }

    fn agent_gui_pointer_pos(
        &self,
        ctx: &egui::Context,
        id: Option<&str>,
        x: Option<f32>,
        y: Option<f32>,
    ) -> Pos2 {
        if let (Some(x), Some(y)) = (x, y) {
            return Pos2::new(x, y);
        }
        if let Some(id) = id
            && let Some(pos) = self.agent_gui_node_center(ctx, id)
        {
            return pos;
        }
        ctx.content_rect().center()
    }

    fn agent_gui_wait_satisfied(
        &self,
        ctx: &egui::Context,
        condition: &AgentGuiWaitCondition,
    ) -> bool {
        match condition {
            AgentGuiWaitCondition::Text { text } => {
                let needle = text.to_ascii_lowercase();
                self.agent_gui_snapshot(ctx)
                    .texts
                    .iter()
                    .any(|candidate| candidate.to_ascii_lowercase().contains(&needle))
            }
            AgentGuiWaitCondition::View { view } => {
                parse_agent_gui_view(view).is_some_and(|target| target == self.current_view)
            }
            AgentGuiWaitCondition::Idle => !self.agent_gui_busy(),
            AgentGuiWaitCondition::Modal { open } => {
                (self.agent_gui_active_modal_count() > 0) == *open
            }
            AgentGuiWaitCondition::Toast { text } => {
                let needle = text.to_ascii_lowercase();
                self.agent_gui_current_toast()
                    .is_some_and(|toast| toast.message.to_ascii_lowercase().contains(&needle))
            }
            AgentGuiWaitCondition::BusyReasonCleared { reason } => {
                let needle = normalize_selector(reason);
                !self
                    .agent_gui_busy_reasons()
                    .iter()
                    .any(|candidate| normalize_selector(candidate) == needle)
            }
            AgentGuiWaitCondition::DownloadComplete => self.download_finished,
            AgentGuiWaitCondition::FpsAbove { fps } => self.fps_ema >= *fps,
            AgentGuiWaitCondition::NodeVisible { id } => !self
                .agent_gui_find_nodes(ctx, None, None, Some(id), true)
                .is_empty(),
        }
    }

    fn agent_gui_snapshot_value(&self, ctx: &egui::Context) -> Value {
        serde_json::to_value(self.agent_gui_snapshot(ctx)).unwrap_or_else(|_| json!({}))
    }

    fn agent_gui_snapshot(&self, ctx: &egui::Context) -> AgentGuiSnapshot {
        let nodes = self.agent_gui_nodes(ctx);
        let mut texts: BTreeSet<String> = nodes
            .iter()
            .map(|node| node.text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        texts.insert("Foxy".to_string());
        texts.insert(self.agent_gui_view_name().to_string());
        for repo in self.repository_view_state.repositories.iter().take(80) {
            if !repo.name.trim().is_empty() {
                texts.insert(repo.name.clone());
            }
        }
        if let Some(repo_index) = self.selected_repository_for_settings
            && let Some(repo) = self.repository_view_state.repositories.get(repo_index)
        {
            texts.insert(repo.name.clone());
        }
        if self.update_modal_open {
            texts.insert("update-modal".to_string());
        }

        let content_rect = ctx.content_rect();
        let modal_names: Vec<String> = self
            .agent_gui_modal_names()
            .into_iter()
            .map(String::from)
            .collect();
        let pointer = ctx
            .pointer_latest_pos()
            .map(|pos| AgentGuiPoint { x: pos.x, y: pos.y });
        let focused = agent_gui_focused_name(ctx);
        AgentGuiSnapshot {
            view: self.agent_gui_view_name().to_string(),
            update_modal_open: self.update_modal_open,
            fps: self.fps_ema,
            startup_frame_rendered: self.startup_frame_rendered,
            busy: self.agent_gui_busy(),
            active_modal_count: modal_names.len(),
            active_modals: modal_names,
            pointer,
            focused,
            repositories_count: self.repository_view_state.repositories.len(),
            selected_repository: self.repository_view_state.selected_repository,
            settings_tab: (self.current_view == FoxyView::Settings)
                .then(|| self.settings_view_state.current_tab.clone()),
            repository_settings_tab: (self.current_view == FoxyView::RepositorySettings)
                .then(|| self.current_repository_settings_tab.as_str().to_string()),
            frame: ctx.cumulative_frame_nr(),
            cumulative_pass_nr: ctx.cumulative_pass_nr(),
            pixels_per_point: ctx.pixels_per_point(),
            zoom_factor: ctx.zoom_factor(),
            busy_reasons: self
                .agent_gui_busy_reasons()
                .into_iter()
                .map(String::from)
                .collect(),
            content_rect: AgentGuiRect {
                x: content_rect.min.x,
                y: content_rect.min.y,
                w: content_rect.width(),
                h: content_rect.height(),
            },
            texts: texts.into_iter().collect(),
            nodes,
        }
    }

    fn agent_gui_find_nodes(
        &self,
        ctx: &egui::Context,
        text: Option<&str>,
        role: Option<&str>,
        id: Option<&str>,
        visible_only: bool,
    ) -> Vec<AgentGuiNode> {
        let text = text.map(|value| value.to_ascii_lowercase());
        let role = role.map(normalize_selector);
        let id = id.map(normalize_selector);
        let content_rect = ctx.content_rect();
        self.agent_gui_nodes(ctx)
            .into_iter()
            .filter(|node| {
                text.as_ref()
                    .is_none_or(|needle| node.text.to_ascii_lowercase().contains(needle))
            })
            .filter(|node| {
                role.as_ref()
                    .is_none_or(|needle| normalize_selector(&node.role) == *needle)
            })
            .filter(|node| {
                id.as_ref()
                    .is_none_or(|needle| normalize_selector(&node.id) == *needle)
            })
            .filter(|node| {
                // A node is "visible" when it has a rect that overlaps the
                // content area; rect-less status nodes are excluded.
                !visible_only
                    || node.rect.as_ref().is_some_and(|rect| {
                        rect.x + rect.w >= content_rect.left()
                            && rect.x <= content_rect.right()
                            && rect.y + rect.h >= content_rect.top()
                            && rect.y <= content_rect.bottom()
                    })
            })
            .collect()
    }

    fn agent_gui_nodes(&self, ctx: &egui::Context) -> Vec<AgentGuiNode> {
        let rect = ctx.content_rect();
        let footer_y = rect.bottom() - self.footer_bar_height();
        let button = |id: &str, text: String, x: f32, y: f32| AgentGuiNode {
            id: id.to_string(),
            role: "button".to_string(),
            text,
            enabled: true,
            focused: false,
            rect: Some(AgentGuiRect {
                x,
                y,
                w: 44.0,
                h: self.footer_bar_height(),
            }),
        };

        let mut nodes = vec![
            AgentGuiNode {
                id: "app.title".to_string(),
                role: "label".to_string(),
                text: "Foxy".to_string(),
                enabled: true,
                focused: false,
                rect: Some(AgentGuiRect {
                    x: rect.left(),
                    y: rect.top(),
                    w: 220.0,
                    h: self.header_bar_height(),
                }),
            },
            button(
                "header.settings",
                self.t("Settings"),
                rect.right() - 52.0,
                rect.top(),
            ),
            button(
                "footer.changelog",
                self.t("Open changelog"),
                rect.left() + 8.0,
                footer_y,
            ),
            button(
                "footer.about",
                self.t("Open about"),
                rect.left() + 72.0,
                footer_y,
            ),
            button(
                "footer.help",
                self.t("Open help"),
                rect.left() + 124.0,
                footer_y,
            ),
            button(
                "footer.activity-log",
                self.t("Show activity log"),
                rect.right() - 56.0,
                footer_y,
            ),
        ];

        nodes.push(AgentGuiNode {
            id: "view.current".to_string(),
            role: "status".to_string(),
            text: self.agent_gui_view_name().to_string(),
            enabled: true,
            focused: false,
            rect: None,
        });
        if self.settings_view_state.show_fps_counter || self.fps_ema > 0.0 {
            nodes.push(AgentGuiNode {
                id: "footer.fps".to_string(),
                role: "status".to_string(),
                text: format!("{:.0} {}", self.fps_ema.round().max(0.0), self.t("FPS")),
                enabled: true,
                focused: false,
                rect: Some(AgentGuiRect {
                    x: rect.right() - 132.0,
                    y: footer_y,
                    w: 72.0,
                    h: self.footer_bar_height(),
                }),
            });
        }
        nodes
    }

    fn agent_gui_view_name(&self) -> &'static str {
        view_to_agent_name(self.current_view)
    }

    fn agent_gui_busy(&self) -> bool {
        !self.agent_gui_busy_reasons().is_empty()
    }

    /// Stable kebab-case names of every background-work flag that currently
    /// makes the app "busy". Single source of truth for the `busy` boolean and
    /// the `busy_reasons` lists in `snapshot`/`progress`, so an agent can see
    /// *why* the UI is not idle rather than only that it isn't.
    fn agent_gui_busy_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        let mut push = |active: bool, name: &'static str| {
            if active {
                reasons.push(name);
            }
        };
        push(self.backend_worker.is_some(), "core-sync");
        push(self.quick_scan_worker.is_some(), "quick-scan");
        push(self.direct_download_worker.is_some(), "direct-download");
        push(
            self.repository_space_import_in_flight,
            "repository-space-import",
        );
        push(self.addon_hash_recalc_in_flight, "addon-hash-recalc");
        push(
            self.backup_inventory_refresh_in_progress,
            "backup-inventory-refresh",
        );
        push(
            self.settings_save_in_flight_revision.is_some(),
            "settings-save",
        );
        push(
            self.repositories_save_in_flight_revision.is_some(),
            "repositories-save",
        );
        reasons
    }

    fn agent_gui_repositories_value(&self, contains: Option<&str>, limit: Option<usize>) -> Value {
        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut rows: Vec<Value> = Vec::new();
        for (index, repo) in self.repository_view_state.repositories.iter().enumerate() {
            if let Some(needle) = &needle {
                let name_match = repo.name.to_ascii_lowercase().contains(needle);
                let address_match = repo.address.to_ascii_lowercase().contains(needle);
                if !name_match && !address_match {
                    continue;
                }
            }
            let instance_key = Self::repo_instance_key(&repo.address, &repo.path);
            let pending = self
                .pending_update_cache
                .get(&instance_key)
                .map(|mods| mods.len())
                .unwrap_or(0);
            rows.push(json!({
                "index": index,
                "name": repo.name,
                "address": repo.address,
                "path": repo.path,
                "state": repo_state_name(self.repo_state_for_address(&repo.address, &repo.path)),
                "selected": self.repository_view_state.selected_repository == Some(index),
                "pending_update_count": pending,
                "addon_count": repo.addons.len(),
                "optional_addon_count": repo.optional_addons.len(),
                "external_addon_count": repo.external_addons.len(),
                "profile_count": repo.profiles.len(),
                "selected_profile": repo.selected_profile,
                "space_id": repo.repository_space_id,
            }));
        }
        let total = rows.len();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        json!({
            "total": total,
            "returned": rows.len(),
            "repositories": rows,
        })
    }

    fn agent_gui_addons_value(
        &self,
        repository_index: Option<usize>,
        tab: Option<&str>,
        contains: Option<&str>,
        enabled_only: bool,
        limit: Option<usize>,
    ) -> Result<Value, String> {
        let repo_index = repository_index
            .or(self.selected_repository_for_settings)
            .or(self.repository_view_state.selected_repository)
            .ok_or_else(|| "Provide --repo-index or select a repository first".to_string())?;
        let repo = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .ok_or_else(|| format!("Repository index {repo_index} is out of range"))?;

        let tab = match tab {
            Some(tab) => parse_agent_gui_repository_settings_tab(tab)
                .ok_or_else(|| format!("Unsupported repository-settings tab '{tab}'"))?,
            None if self.current_view == FoxyView::RepositorySettings => {
                self.current_repository_settings_tab
            }
            None => RepositorySettingsTab::Addons,
        };

        let needle = contains.map(|value| value.to_ascii_lowercase());
        let keep = |name: &str, enabled: bool| -> bool {
            if enabled_only && !enabled {
                return false;
            }
            needle
                .as_ref()
                .is_none_or(|needle| name.to_ascii_lowercase().contains(needle))
        };

        let mut rows: Vec<Value> = Vec::new();
        match tab {
            RepositorySettingsTab::Configuration | RepositorySettingsTab::Addons => {
                for (name, enabled) in &repo.addons {
                    if !keep(name, *enabled) {
                        continue;
                    }
                    rows.push(json!({
                        "name": name,
                        "enabled": enabled,
                        "kind": "required",
                        "size_bytes": self.repository_addon_remote_size_bytes(&repo.address, name),
                    }));
                }
            }
            RepositorySettingsTab::OptionalAddons => {
                for (name, enabled) in &repo.optional_addons {
                    if !keep(name, *enabled) {
                        continue;
                    }
                    rows.push(json!({
                        "name": name,
                        "enabled": enabled,
                        "kind": "optional",
                        "favorite": repo.optional_addon_favorites.iter().any(|f| f == name),
                        "client_side": repo.optional_addon_client_side.iter().any(|c| c == name),
                        "size_bytes": self.repository_addon_remote_size_bytes(&repo.address, name),
                    }));
                }
            }
            RepositorySettingsTab::ExternalAddons => {
                for (name, enabled, source) in &repo.external_addons {
                    if !keep(name, *enabled) {
                        continue;
                    }
                    rows.push(json!({
                        "name": name,
                        "enabled": enabled,
                        "kind": "external",
                        "source": source,
                        "favorite": repo.external_addon_favorites.iter().any(|f| f == name),
                        "client_side": repo.external_addon_client_side.iter().any(|c| c == name),
                        "size_bytes": self.repository_addon_remote_size_bytes(&repo.address, name),
                    }));
                }
            }
        }

        let total = rows.len();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(json!({
            "repository_index": repo_index,
            "repository_name": repo.name,
            "tab": tab.as_str(),
            "total": total,
            "returned": rows.len(),
            "addons": rows,
        }))
    }

    /// Structured launch profiles for a repository. Profiles drive launch
    /// behavior and are not exposed as semantic nodes, so this is the only way
    /// to read their flags/overrides programmatically.
    fn agent_gui_profiles_value(
        &self,
        repository_index: Option<usize>,
        contains: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Value, String> {
        let repo_index = repository_index
            .or(self.selected_repository_for_settings)
            .or(self.repository_view_state.selected_repository)
            .ok_or_else(|| "Provide --repo-index or select a repository first".to_string())?;
        let repo = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .ok_or_else(|| format!("Repository index {repo_index} is out of range"))?;

        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut rows: Vec<Value> = Vec::new();
        for profile in &repo.profiles {
            if let Some(needle) = &needle
                && !profile.name.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            rows.push(json!({
                "name": profile.name,
                "selected": repo.selected_profile.as_deref() == Some(profile.name.as_str()),
                "flags": {
                    "csla": profile.csla,
                    "ef": profile.ef,
                    "gm": profile.gm,
                    "rf": profile.rf,
                    "spe": profile.spe,
                    "vn": profile.vn,
                    "ws": profile.ws,
                    "skip_intro": profile.skip_intro,
                    "no_splash": profile.no_splash,
                    "world_empty": profile.world_empty,
                    "load_mission_to_memory": profile.load_mission_to_memory,
                    "enable_ht": profile.enable_ht,
                    "huge_pages": profile.huge_pages,
                    "no_logs": profile.no_logs,
                    "include_steam_addons": profile.include_steam_addons,
                },
                "additional_params": profile.additional_params,
                "addon_override_count": profile.addons.len(),
                "optional_addon_override_count": profile.optional_addons.len(),
                "external_addon_override_count": profile.external_addons.len(),
            }));
        }

        let total = rows.len();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(json!({
            "repository_index": repo_index,
            "repository_name": repo.name,
            "selected_profile": repo.selected_profile,
            "total": total,
            "returned": rows.len(),
            "profiles": rows,
        }))
    }

    /// The cached editor missions for the currently viewed repository. Exposes
    /// name/folder/terrain without absolute paths (per the harness security
    /// rules); `null` until a repository view has populated `cached_missions`.
    fn agent_gui_missions_value(&self, contains: Option<&str>, limit: Option<usize>) -> Value {
        let Some(cached) = self.cached_missions.as_ref() else {
            return json!({
                "loaded": false,
                "total": 0,
                "returned": 0,
                "missions": [],
            });
        };

        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut rows: Vec<Value> = Vec::new();
        for mission in &cached.missions {
            if let Some(needle) = &needle {
                let name_match = mission.display_name.to_ascii_lowercase().contains(needle);
                let folder_match = mission.folder_name.to_ascii_lowercase().contains(needle);
                let world_match = mission.world_name.to_ascii_lowercase().contains(needle);
                if !name_match && !folder_match && !world_match {
                    continue;
                }
            }
            rows.push(json!({
                "display_name": mission.display_name,
                "folder_name": mission.folder_name,
                "world_name": mission.world_name,
                "root_folder_name": mission.root_folder_name,
                "is_multiplayer": mission.is_multiplayer,
                "author": mission.author,
                "game_type": mission.game_type,
                "max_players": mission.max_players,
            }));
        }

        let total = rows.len();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        json!({
            "loaded": true,
            "profile_name": cached.profile_name,
            "scanned_age_ms": cached.scanned_at.elapsed().as_millis() as u64,
            "total": total,
            "returned": rows.len(),
            "missions": rows,
        })
    }

    /// Repository spaces with attached-repository counts, the selected space, and
    /// any in-flight bulk-action progress. Avoids dumping on-disk path fields.
    fn agent_gui_spaces_value(&self, contains: Option<&str>, limit: Option<usize>) -> Value {
        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut rows: Vec<Value> = Vec::new();
        for space in &self.repository_spaces {
            if let Some(needle) = &needle
                && !space.name.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            let attached = self
                .repository_view_state
                .repositories
                .iter()
                .filter(|repo| repo.repository_space_id.as_deref() == Some(space.id.as_str()))
                .count();
            let required_entries = space.entries.iter().filter(|entry| entry.required).count();
            rows.push(json!({
                "id": space.id,
                "name": space.name,
                "local_name_override": space.local_name_override,
                "collapsed": space.collapsed,
                "selected": self.selected_repository_space_id.as_deref() == Some(space.id.as_str()),
                "manifest_entry_count": space.entries.len(),
                "required_entry_count": required_entries,
                "attached_repository_count": attached,
            }));
        }

        let bulk_progress = self
            .repository_space_bulk_progress
            .as_ref()
            .map(|progress| {
                json!({
                    "space_id": progress.space_id,
                    "mode": format!("{:?}", progress.mode),
                    "total_count": progress.total_count,
                    "completed_count": progress.completed_count,
                    "succeeded_count": progress.succeeded_count,
                    "failed_count": progress.failed_count,
                    "updates_available_count": progress.updates_available_count,
                    "up_to_date_count": progress.up_to_date_count,
                    "current_repo_name": progress.current_repo_name,
                })
            });

        let total = rows.len();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        json!({
            "selected_space": self.selected_repository_space_id,
            "bulk_progress": bulk_progress,
            "total": total,
            "returned": rows.len(),
            "spaces": rows,
        })
    }

    /// The last completed `DownloadSummary`. Telemetry samples can be large, so
    /// they are summarized to counts unless `include_telemetry` is set.
    fn agent_gui_download_summary_value(&self, include_telemetry: bool) -> Value {
        let Some(summary) = self.download_summary.as_ref() else {
            return json!({ "present": false });
        };
        let mut value = json!({
            "present": true,
            "mods_updated": summary.mods_updated,
            "files_updated": summary.files_updated,
            "parts_updated": summary.parts_updated,
            "downloaded_bytes": summary.downloaded_bytes,
            "planned_transfer_bytes": summary.planned_transfer_bytes,
            "full_download_bytes": summary.full_download_bytes,
            "patch_savings_bytes": summary.patch_savings_bytes,
            "patched_files": summary.patched_files,
            "download_stage_ms": summary.download_stage_duration.as_millis() as u64,
            "hash_stage_ms": summary.hash_stage_duration.as_millis() as u64,
            "cumulative_hash_ms": summary.cumulative_hash_duration.as_millis() as u64,
            "after_download_hash_ms": summary.after_download_hash_duration.as_millis() as u64,
            "total_ms": summary.total_duration.as_millis() as u64,
            "avg_speed_bps": summary.avg_speed_bps,
            "telemetry_sample_count": summary.telemetry_samples.len(),
        });
        if include_telemetry && let Value::Object(map) = &mut value {
            map.insert(
                "telemetry_samples".to_string(),
                serde_json::to_value(&summary.telemetry_samples).unwrap_or_else(|_| json!([])),
            );
        }
        value
    }

    /// The current user-feedback toast, if one is showing.
    fn agent_gui_toasts_value(&self) -> Value {
        match self.agent_gui_current_toast() {
            Some(toast) => {
                let age = toast.shown_at.elapsed();
                let remaining = toast.duration.saturating_sub(age);
                json!({
                    "present": true,
                    "message": toast.message,
                    "kind": toast_kind_name(toast.kind),
                    "age_ms": age.as_millis() as u64,
                    "remaining_ms": remaining.as_millis() as u64,
                    "duration_ms": toast.duration.as_millis() as u64,
                })
            }
            None => json!({ "present": false }),
        }
    }

    /// The toast Foxy would still be rendering this frame: the stored toast if it
    /// has not yet outlived its display duration. Single source of truth for the
    /// `toasts` fetch and the `wait --toast` predicate.
    fn agent_gui_current_toast(&self) -> Option<&crate::ui::app::UiToastState> {
        self.ui_toast
            .as_ref()
            .filter(|toast| toast.shown_at.elapsed() < toast.duration)
    }

    /// Mutate a single live setting on the running app, clamping/validating the
    /// way the offline `settings set` CLI does. Returns the applied value so an
    /// agent can confirm clamping. Loopback + token-gated like every command.
    fn agent_gui_set_setting(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        value: &str,
    ) -> Result<Value, String> {
        let trimmed = value.trim();
        let applied = match normalize_selector(key).as_str() {
            "debug-mode" | "debug_mode" => {
                let parsed = parse_agent_gui_bool(trimmed)
                    .ok_or_else(|| format!("Expected a boolean for debug-mode, got '{value}'"))?;
                self.settings_view_state.debug_mode = parsed;
                self.mark_settings_dirty();
                json!(parsed)
            }
            "show-activity-log" | "show_activity_log" => {
                let parsed = parse_agent_gui_bool(trimmed).ok_or_else(|| {
                    format!("Expected a boolean for show-activity-log, got '{value}'")
                })?;
                // Routes through the dedicated helper so the panel relayout and
                // settings save fire exactly as the footer toggle would.
                self.set_activity_log_visibility(ctx, parsed, "agent GUI driver");
                json!(parsed)
            }
            "show-fps-counter" | "show_fps_counter" => {
                let parsed = parse_agent_gui_bool(trimmed).ok_or_else(|| {
                    format!("Expected a boolean for show-fps-counter, got '{value}'")
                })?;
                self.settings_view_state.show_fps_counter = parsed;
                self.mark_settings_dirty();
                json!(parsed)
            }
            "ui-scale-percent" | "ui_scale_percent" => {
                let parsed: u16 = trimmed.parse().map_err(|_| {
                    format!("Expected an integer percent for ui-scale-percent, got '{value}'")
                })?;
                json!(self.agent_gui_set_scale(ctx, parsed))
            }
            "locale" => {
                self.settings_view_state.locale = trimmed.to_string();
                self.i18n.set_language(trimmed);
                self.mark_settings_dirty();
                ctx.request_repaint();
                json!(trimmed)
            }
            "download-speed-limit-mbps" | "download_speed_limit_mbps" => {
                if trimmed.eq_ignore_ascii_case("unlimited")
                    || trimmed.eq_ignore_ascii_case("none")
                    || trimmed.is_empty()
                {
                    self.settings_view_state.download_speed_limit_mbps = None;
                    self.mark_settings_dirty();
                    Value::Null
                } else {
                    let parsed: u32 = trimmed.parse().map_err(|_| {
                        format!(
                            "Expected an integer Mbps or 'unlimited' for download-speed-limit-mbps, got '{value}'"
                        )
                    })?;
                    // Mirror the offline CLI: the minimum applied limit is 1 Mbps.
                    let clamped = parsed.max(1);
                    self.settings_view_state.download_speed_limit_mbps = Some(clamped);
                    self.mark_settings_dirty();
                    json!(clamped)
                }
            }
            other => {
                return Err(format!(
                    "Unsupported setting '{other}' (try debug-mode, show-activity-log, show-fps-counter, ui-scale-percent, locale, or download-speed-limit-mbps)"
                ));
            }
        };
        ctx.request_repaint();
        Ok(json!({ "key": normalize_selector(key), "value": applied }))
    }

    /// Serialize the live, effective settings the running app currently holds.
    /// Mirrors `settings show` but reads in-memory state rather than the file.
    fn agent_gui_settings_value(&self) -> Value {
        serde_json::to_value(&self.settings_view_state).unwrap_or_else(|_| json!({}))
    }

    fn agent_gui_progress_value(&self) -> Value {
        let download_label = self
            .download_progress
            .as_ref()
            .map(|(label, _)| label.clone());
        let download_percent = self.download_progress.as_ref().map(|(_, percent)| *percent);
        let sync_mode = self.current_sync_mode.map(|mode| format!("{mode:?}"));
        let recheck_hash = self
            .recheck_hash_counter
            .map(|(done, total)| json!({ "done": done, "total": total }));
        json!({
            "busy": self.agent_gui_busy(),
            "busy_reasons": self.agent_gui_busy_reasons(),
            "syncing_repository": self.syncing_repository,
            "current_sync_mode": sync_mode,
            "download_active": self.download_progress.is_some(),
            "download_label": download_label,
            "download_percent": download_percent,
            "download_paused": self.download_paused,
            "download_finished": self.download_finished,
            "download_speed_bps": self.download_speed_bps,
            "download_eta_secs": self.download_eta_remaining.map(|d| d.as_secs_f64()),
            "total_downloaded_bytes": self.total_downloaded_bytes,
            "active_mod_downloads": self.mod_download_progress.len(),
            "recheck_stage_label": self.recheck_stage_label,
            "recheck_stage_percent": self.recheck_stage_percent,
            "recheck_hash_counter": recheck_hash,
            "update_modal_open": self.update_modal_open,
        })
    }

    /// Apply a global UI scale the way the settings slider's Apply button does:
    /// set `ui_scale_percent` (clamped to the slider range) and mark settings
    /// dirty; `apply_runtime_ui_scale` picks it up on the next frame.
    fn agent_gui_set_scale(&mut self, ctx: &egui::Context, percent: u16) -> u16 {
        let clamped = percent.clamp(MIN_UI_SCALE_PERCENT, MAX_UI_SCALE_PERCENT);
        self.settings_view_state.ui_scale_percent = clamped;
        self.settings_view_state.ui_scale_percent_draft = clamped;
        self.mark_settings_dirty();
        ctx.request_repaint();
        clamped
    }

    fn agent_gui_active_modal_count(&self) -> usize {
        self.agent_gui_modal_names().len()
    }

    /// Stable kebab-case names of every modal/dialog Foxy currently treats as
    /// open. Single source of truth for both the count and the snapshot list.
    fn agent_gui_modal_names(&self) -> Vec<&'static str> {
        let mut modals = Vec::new();
        let mut push = |active: bool, name: &'static str| {
            if active {
                modals.push(name);
            }
        };
        push(self.update_modal_open, "update");
        push(self.show_add_repository_modal, "add-repository");
        push(
            self.pending_repository_duplicate_add.is_some(),
            "repository-duplicate",
        );
        push(
            self.pending_mission_duplicate.is_some(),
            "mission-duplicate",
        );
        push(self.pending_mission_delete.is_some(), "mission-delete");
        push(
            self.pending_mission_remove_dependencies.is_some(),
            "mission-remove-dependencies",
        );
        push(
            self.pending_mission_editor_launch_warning.is_some(),
            "mission-editor-launch-warning",
        );
        push(
            self.pending_addon_destructive_confirmation.is_some(),
            "addon-destructive-confirmation",
        );
        push(
            self.pending_settings_folder_removal.is_some(),
            "settings-folder-removal",
        );
        push(self.pending_join_preflight.is_some(), "join-preflight");
        push(
            self.pending_repository_space_bulk_action.is_some(),
            "repository-space-bulk-action",
        );
        push(
            self.pending_repository_space_delete_id.is_some(),
            "repository-space-delete",
        );
        push(
            self.pending_renderer_fallback_notice,
            "renderer-fallback-notice",
        );
        push(self.pending_db_schema_wipe.is_some(), "db-schema-wipe");
        push(self.pending_app_update_prompt, "app-update-available");
        push(self.show_add_profile_window, "add-profile");
        push(self.show_rename_profile_window, "rename-profile");
        push(
            self.pending_profile_confirm_action.is_some(),
            "profile-confirm-action",
        );
        push(
            self.pending_settings_reset_confirmation,
            "settings-reset-confirmation",
        );
        push(self.show_memory_diagnostics_window, "memory-diagnostics");
        modals
    }

    /// Build/version + renderer preflight. The natural first call of a session
    /// and a single CI gate for client/server version mismatch.
    fn agent_gui_health_value(&self, runtime: &AgentGuiRuntime) -> Value {
        json!({
            "version": crate::build_info::VERSION,
            "version_label": crate::build_info::version_label(),
            "commit": crate::build_info::GIT_HASH,
            "build_kind": crate::build_info::build_kind(),
            "official_build": crate::build_info::is_official_build(),
            "dev_build": crate::build_info::is_dev_build(),
            "agent_gui": true,
            "renderer": runtime.active_renderer.unwrap_or("unknown"),
            "renderer_preference": format!("{:?}", self.settings_view_state.ui_renderer)
                .to_ascii_lowercase(),
            "renderer_fallback_pending": self.pending_renderer_fallback_notice,
            "stable_render": runtime.stable_render,
            "locale": self.settings_view_state.locale,
            "uptime_ms": runtime.started_at.elapsed().as_millis() as u64,
            "startup_frame_rendered": self.startup_frame_rendered,
        })
    }

    /// Set keyboard focus on a named text field (or clear it). The reachable
    /// targets are the widgets instrumented with a stable agent egui id;
    /// text-backed fields without one are driven via `fill`/`set-filter`.
    fn agent_gui_focus(
        &mut self,
        ctx: &egui::Context,
        target: Option<&str>,
        clear: bool,
    ) -> Result<Value, String> {
        if clear || target.map(normalize_selector).as_deref() == Some("none") {
            ctx.memory_mut(|memory| memory.stop_text_input());
            ctx.request_repaint();
            return Ok(json!({ "focused": Value::Null, "cleared": true }));
        }
        let target = target.ok_or_else(|| "Provide --target <name> or --clear".to_string())?;
        let normalized = normalize_selector(target);
        let id = agent_gui_focus_target_id(&normalized).ok_or_else(|| {
            format!(
                "No focusable widget registered for target '{target}'. Registered: {}. Use fill/set-filter for other text fields.",
                agent_gui_focus_target_names().join(", ")
            )
        })?;
        // The widget must actually render this frame, or egui reports a focused
        // id that AccessKit's tree does not contain and the UI thread panics
        // (`Focused ID ... is not in the node list`). `poll_agent_gui` runs
        // before the views draw, so opening the container here makes the widget
        // present in the same frame it claims focus.
        self.agent_gui_ensure_focus_target_visible(&normalized);
        ctx.memory_mut(|memory| memory.request_focus(id));
        ctx.request_repaint();
        Ok(json!({ "focused": normalized, "requested": true }))
    }

    /// Make a focus target's container visible so the widget renders this frame
    /// (see the AccessKit panic note in `agent_gui_focus`).
    fn agent_gui_ensure_focus_target_visible(&mut self, normalized_target: &str) {
        if normalized_target == "add-repository-input" {
            self.show_add_repository_modal = true;
        }
    }

    /// Focus + clear + set a named text field by writing its backing app state
    /// directly. More reliable than focus-then-type (no per-key event timing),
    /// and the box renders the new value on the next frame regardless of focus.
    fn agent_gui_fill(
        &mut self,
        ctx: &egui::Context,
        target: &str,
        value: &str,
    ) -> Result<Value, String> {
        let canonical = self.agent_gui_set_text_field(target, value)?;
        // If the field has a registered egui id, request focus too so a human
        // re-running the same step sees a caret in the box. Make the container
        // visible first to avoid the AccessKit focus panic (see `agent_gui_focus`).
        let normalized = normalize_selector(target);
        if let Some(id) = agent_gui_focus_target_id(&normalized) {
            self.agent_gui_ensure_focus_target_visible(&normalized);
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        ctx.request_repaint();
        Ok(json!({ "target": canonical, "value": value }))
    }

    /// Write one named text field's backing `String`, returning its canonical
    /// kebab-case name. Shared by `fill`. Filter-only writes go through
    /// `agent_gui_set_filter`.
    fn agent_gui_set_text_field(
        &mut self,
        name: &str,
        value: &str,
    ) -> Result<&'static str, String> {
        let value = value.to_string();
        let canonical = match normalize_selector(name).as_str() {
            "add-repository-input" | "add-repository" => {
                self.add_repository_input_address = value;
                "add-repository-input"
            }
            "profile-name" | "new-profile-name" => {
                self.new_profile_name = value;
                "profile-name"
            }
            "addons-filter" => {
                self.addons_filter = value;
                "addons-filter"
            }
            "optional-addons-filter" => {
                self.optional_addons_filter = value;
                "optional-addons-filter"
            }
            "external-addons-filter" => {
                self.external_addons_filter = value;
                "external-addons-filter"
            }
            "external-addons-origin-filter" => {
                self.external_addons_origin_filter = value;
                "external-addons-origin-filter"
            }
            "addon-state-filter" => {
                self.addon_state_filter = value;
                "addon-state-filter"
            }
            "mission-search" | "editor-mission-search" => {
                self.editor_mission_search = value;
                "mission-search"
            }
            "mission-terrain-filter" | "editor-mission-terrain-filter" => {
                self.editor_mission_terrain_filter = value;
                "mission-terrain-filter"
            }
            "space-detail-filter" | "repository-space-detail-filter" => {
                self.repository_space_detail_filter = value;
                "space-detail-filter"
            }
            "backup-filter" | "backup-manager-filter" => {
                self.backup_manager_filter = value;
                "backup-filter"
            }
            "direct-download-url" => {
                self.direct_download_url_input = value;
                "direct-download-url"
            }
            "direct-download-destination" => {
                self.direct_download_destination_input = value;
                "direct-download-destination"
            }
            other => {
                return Err(format!(
                    "Unknown text field '{other}' (try add-repository-input, profile-name, addons-filter, optional-addons-filter, external-addons-filter, external-addons-origin-filter, addon-state-filter, mission-search, mission-terrain-filter, space-detail-filter, backup-filter, direct-download-url, direct-download-destination)"
                ));
            }
        };
        Ok(canonical)
    }

    /// Read every repository/addon/mission/space list filter the harness can
    /// drive, so an agent can assert filter state and restore it.
    fn agent_gui_filters_value(&self) -> Value {
        json!({
            "addons_filter": self.addons_filter,
            "optional_addons_filter": self.optional_addons_filter,
            "external_addons_filter": self.external_addons_filter,
            "external_addons_origin_filter": self.external_addons_origin_filter,
            "external_addons_group_by_origin": self.external_addons_group_by_origin,
            "addons_search_files": self.addons_search_files,
            "optional_addons_search_files": self.optional_addons_search_files,
            "external_addons_search_files": self.external_addons_search_files,
            "addon_state_filter": self.addon_state_filter,
            "addon_favorites_only_filter": self.addon_favorites_only_filter,
            "addon_client_side_only_filter": self.addon_client_side_only_filter,
            "editor_mission_search": self.editor_mission_search,
            "editor_mission_terrain_filter": self.editor_mission_terrain_filter,
            "editor_mission_show_folders": self.editor_mission_show_folders,
            "repository_space_detail_filter": self.repository_space_detail_filter,
            "backup_manager_filter": self.backup_manager_filter,
        })
    }

    /// Write one list filter (string or boolean) and request a repaint. The
    /// reliable way to drive the addon-list scroll/galley recipes.
    fn agent_gui_set_filter(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        value: &str,
    ) -> Result<Value, String> {
        let normalized = normalize_selector(name);
        let bool_filter = |raw: &str, field: &str| -> Result<bool, String> {
            parse_agent_gui_bool(raw)
                .ok_or_else(|| format!("Expected a boolean for {field}, got '{value}'"))
        };
        let applied = match normalized.as_str() {
            "favorites-only" | "addon-favorites-only-filter" | "addon-favorites-only" => {
                let parsed = bool_filter(value, "favorites-only")?;
                self.addon_favorites_only_filter = parsed;
                json!(parsed)
            }
            "client-side-only" | "addon-client-side-only-filter" | "addon-client-side-only" => {
                let parsed = bool_filter(value, "client-side-only")?;
                self.addon_client_side_only_filter = parsed;
                json!(parsed)
            }
            "group-by-origin" | "external-addons-group-by-origin" => {
                let parsed = bool_filter(value, "group-by-origin")?;
                self.external_addons_group_by_origin = parsed;
                json!(parsed)
            }
            "addons-search-files" | "addons-include-files" => {
                let parsed = bool_filter(value, "addons-search-files")?;
                self.addons_search_files = parsed;
                json!(parsed)
            }
            "optional-addons-search-files" | "optional-addons-include-files" => {
                let parsed = bool_filter(value, "optional-addons-search-files")?;
                self.optional_addons_search_files = parsed;
                json!(parsed)
            }
            "external-addons-search-files" | "external-addons-include-files" => {
                let parsed = bool_filter(value, "external-addons-search-files")?;
                self.external_addons_search_files = parsed;
                json!(parsed)
            }
            "show-folders" | "editor-mission-show-folders" => {
                let parsed = bool_filter(value, "show-folders")?;
                self.editor_mission_show_folders = parsed;
                json!(parsed)
            }
            "addons-filter"
            | "optional-addons-filter"
            | "external-addons-filter"
            | "external-addons-origin-filter"
            | "addon-state-filter"
            | "mission-search"
            | "editor-mission-search"
            | "mission-terrain-filter"
            | "editor-mission-terrain-filter"
            | "space-detail-filter"
            | "repository-space-detail-filter" => {
                self.agent_gui_set_text_field(&normalized, value)?;
                json!(value)
            }
            other => {
                return Err(format!(
                    "Unsupported filter '{other}' (string: addons-filter, optional-addons-filter, external-addons-filter, external-addons-origin-filter, addon-state-filter, mission-search, mission-terrain-filter, space-detail-filter; boolean: favorites-only, client-side-only, group-by-origin, addons-search-files, optional-addons-search-files, external-addons-search-files, show-folders)"
                ));
            }
        };
        ctx.request_repaint();
        Ok(json!({ "filter": normalized, "value": applied }))
    }

    /// Non-destructive UI selection: highlight/view a repository, server,
    /// mission, or space the way clicking the row does, without coordinates.
    /// Never reaches a core action.
    fn agent_gui_select(
        &mut self,
        ctx: &egui::Context,
        repository: Option<usize>,
        server: Option<usize>,
        mission: Option<usize>,
        space: Option<&str>,
    ) -> Result<Value, String> {
        let mut changed = serde_json::Map::new();
        if let Some(index) = repository {
            if index >= self.repository_view_state.repositories.len() {
                return Err(format!("Repository index {index} is out of range"));
            }
            self.repository_view_state.selected_repository = Some(index);
            self.selected_repository_for_settings = Some(index);
            changed.insert("repository".to_string(), json!(index));
        }
        if let Some(index) = server {
            let repo_index = self
                .repository_view_state
                .selected_repository
                .ok_or_else(|| "Select a repository before selecting a server".to_string())?;
            let server_count = self
                .repository_view_state
                .repositories
                .get(repo_index)
                .map(|repo| repo.servers.len())
                .unwrap_or(0);
            if index >= server_count {
                return Err(format!(
                    "Server index {index} is out of range (repo has {server_count})"
                ));
            }
            self.repository_selection = Some(RepositorySelection::Server(index));
            changed.insert("server".to_string(), json!(index));
        }
        if let Some(index) = mission {
            let mission_count = self
                .cached_missions
                .as_ref()
                .map(|cached| cached.missions.len())
                .unwrap_or(0);
            if index >= mission_count {
                return Err(format!(
                    "Mission index {index} is out of range (cached list has {mission_count})"
                ));
            }
            self.repository_selection = Some(RepositorySelection::Mission(index));
            changed.insert("mission".to_string(), json!(index));
        }
        if let Some(space_id) = space {
            let exists = self
                .repository_spaces
                .iter()
                .any(|candidate| candidate.id == space_id);
            if !exists {
                return Err(format!("No repository space with id '{space_id}'"));
            }
            self.selected_repository_space_id = Some(space_id.to_string());
            changed.insert("space".to_string(), json!(space_id));
        }
        if changed.is_empty() {
            return Err("Provide --repository, --server, --mission, or --space".to_string());
        }
        ctx.request_repaint();
        Ok(Value::Object(changed))
    }

    /// Window/tray lifecycle routed through `ViewportCommand` and the tray
    /// manager, mirroring the user-triggered paths.
    fn agent_gui_window(&mut self, ctx: &egui::Context, action: &str) -> Result<Value, String> {
        match normalize_selector(action).as_str() {
            "minimize" => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
            "restore" | "show" => {
                if let Some(tray_manager) = self.tray_manager.as_ref() {
                    tray_manager.hide_icon();
                }
                self.hidden_to_tray = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            "maximize" => ctx.send_viewport_cmd(ViewportCommand::Maximized(true)),
            "unmaximize" => ctx.send_viewport_cmd(ViewportCommand::Maximized(false)),
            "focus" => ctx.send_viewport_cmd(ViewportCommand::Focus),
            "hide-to-tray" | "tray" | "hide" => {
                self.hide_app_to_tray(ctx, "agent GUI driver");
            }
            other => {
                return Err(format!(
                    "Unsupported window action '{other}' (use minimize, restore, maximize, unmaximize, focus, hide-to-tray, or show)"
                ));
            }
        }
        ctx.request_repaint();
        Ok(json!({
            "action": normalize_selector(action),
            "hidden_to_tray": self.hidden_to_tray,
        }))
    }

    /// Apply (or clear) stable-render mode: zero egui's animation time and
    /// disable caret blink so screenshots are byte-stable. Re-applied per frame
    /// while on (egui resets style each pass). Lighter than a full clock freeze.
    fn apply_agent_gui_stable_render(&self, ctx: &egui::Context, on: bool) {
        let animation_time = if on {
            0.0
        } else {
            egui::Style::default().animation_time
        };
        ctx.global_style_mut(|style| {
            style.animation_time = animation_time;
            style.visuals.text_cursor.blink = !on;
        });
    }

    /// Evaluate one declarative assertion over an observed field. Sources:
    /// `snapshot` (default), `settings.*`, `progress.*`. Returns ok/fail with
    /// the observed-vs-expected values so scripts fail fast with a clear diff.
    fn agent_gui_assert_value(
        &self,
        ctx: &egui::Context,
        field: &str,
        equals: Option<&str>,
        contains: Option<&str>,
        _repository_index: Option<usize>,
    ) -> Value {
        let (source_value, pointer, source) = match field.split_once('.') {
            Some(("settings", rest)) => (
                self.agent_gui_settings_value(),
                rest.to_string(),
                "settings",
            ),
            Some(("progress", rest)) => (
                self.agent_gui_progress_value(),
                rest.to_string(),
                "progress",
            ),
            Some(("snapshot", rest)) => (
                self.agent_gui_snapshot_value(ctx),
                rest.to_string(),
                "snapshot",
            ),
            _ => (
                self.agent_gui_snapshot_value(ctx),
                field.to_string(),
                "snapshot",
            ),
        };
        let observed = json_pointer_lookup(&source_value, &pointer);
        let observed_text = observed.as_ref().map(json_value_to_plain_string);
        let (op, expected, ok) = if let Some(expected) = equals {
            let ok = observed_text.as_deref() == Some(expected);
            ("equals", Some(expected.to_string()), ok)
        } else if let Some(needle) = contains {
            let ok = observed_text
                .as_deref()
                .is_some_and(|text| text.contains(needle));
            ("contains", Some(needle.to_string()), ok)
        } else {
            (
                "present",
                None,
                observed.as_ref().is_some_and(|v| !v.is_null()),
            )
        };
        json!({
            "ok": ok,
            "field": field,
            "source": source,
            "op": op,
            "expected": expected,
            "observed": observed.unwrap_or(Value::Null),
        })
    }

    /// Global cross-folder addon inventory: which addons are shared across
    /// repositories/folders. On-disk absolute paths are redacted to basenames
    /// (harness security rule); sizes are summed across the returned rows.
    fn agent_gui_inventory_value(
        &self,
        contains: Option<&str>,
        folder: Option<&str>,
        source: Option<&str>,
        limit: Option<usize>,
    ) -> Value {
        // Prefer the already-scanned cache; fall back to a fresh read-only scan.
        let owned;
        let entries: &Vec<AddonInventoryEntry> = match self.cached_all_addons.as_ref() {
            Some(entries) => entries,
            None => {
                owned = self.gather_all_addon_origins();
                &owned
            }
        };
        let name_needle = contains.map(|value| value.to_ascii_lowercase());
        let folder_needle = folder.map(|value| value.to_ascii_lowercase());
        let source_needle = source.map(|value| value.to_ascii_lowercase());

        let mut rows: Vec<Value> = Vec::new();
        let mut total_size: u64 = 0;
        let mut matched = 0usize;
        for (name, path, origin, size) in entries.iter() {
            let folder_name = path_basename(path);
            if let Some(needle) = &name_needle
                && !name.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            if let Some(needle) = &folder_needle
                && !folder_name.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            if let Some(needle) = &source_needle
                && !origin.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            matched += 1;
            total_size = total_size.saturating_add(size.unwrap_or(0));
            if limit.is_none_or(|limit| rows.len() < limit) {
                rows.push(json!({
                    "name": name,
                    "folder": folder_name,
                    "source": origin,
                    "size_bytes": size,
                }));
            }
        }
        json!({
            "total": matched,
            "returned": rows.len(),
            "total_size_bytes": total_size,
            "addons": rows,
        })
    }

    /// The planned update set *before* a sync (`pending_update_cache`), so an
    /// agent can assert the plan, kick a download, then diff plan vs result
    /// against `download-summary`.
    fn agent_gui_pending_updates_value(
        &self,
        repository_index: Option<usize>,
        contains: Option<&str>,
        limit: Option<usize>,
        include_files: bool,
    ) -> Value {
        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut repos: Vec<Value> = Vec::new();
        let mut grand_total_bytes: u64 = 0;
        let mut grand_total_mods = 0usize;
        for (index, repo) in self.repository_view_state.repositories.iter().enumerate() {
            if let Some(target) = repository_index
                && index != target
            {
                continue;
            }
            let instance_key = Self::repo_instance_key(&repo.address, &repo.path);
            let Some(mods) = self.pending_update_cache.get(&instance_key) else {
                continue;
            };
            let mut mod_rows: Vec<Value> = Vec::new();
            let mut repo_bytes: u64 = 0;
            let mut needing_update = 0usize;
            for diff in mods {
                if let Some(needle) = &needle
                    && !diff.name.to_ascii_lowercase().contains(needle)
                {
                    continue;
                }
                repo_bytes = repo_bytes.saturating_add(diff.total_bytes);
                if diff.needs_update {
                    needing_update += 1;
                }
                let mut row = json!({
                    "name": diff.name,
                    "needs_update": diff.needs_update,
                    "total_bytes": diff.total_bytes,
                    "changed_file_count": diff.files.iter().filter(|f| f.needs_update).count(),
                    "file_count": diff.files.len(),
                });
                if include_files && let Value::Object(map) = &mut row {
                    map.insert(
                        "files".to_string(),
                        serde_json::to_value(&diff.files).unwrap_or_else(|_| json!([])),
                    );
                }
                if limit.is_none_or(|limit| mod_rows.len() < limit) {
                    mod_rows.push(row);
                }
            }
            grand_total_bytes = grand_total_bytes.saturating_add(repo_bytes);
            grand_total_mods += mods.len();
            repos.push(json!({
                "repository_index": index,
                "repository_name": repo.name,
                "mod_count": mods.len(),
                "needs_update_count": needing_update,
                "total_bytes": repo_bytes,
                "mods": mod_rows,
            }));
        }
        json!({
            "repositories_with_pending": repos.len(),
            "total_mod_count": grand_total_mods,
            "total_bytes": grand_total_bytes,
            "repositories": repos,
        })
    }

    /// The self-update flow status (`app_update_status`) plus the configured
    /// mode/url, so check → available → download can be driven and observed.
    fn agent_gui_app_update_value(&self) -> Value {
        use crate::core::tasks::app_update::UpdateCheckStatus;
        let (status, detail) = match &self.app_update_status {
            UpdateCheckStatus::Idle => ("idle", Value::Null),
            UpdateCheckStatus::Checking => ("checking", Value::Null),
            UpdateCheckStatus::Available(info) => (
                "available",
                json!({
                    "latest": info.manifest.latest,
                    "current_version": info.current_version,
                    "version_count": info.manifest.versions.len(),
                }),
            ),
            UpdateCheckStatus::Downloading {
                progress,
                bytes_done,
                bytes_total,
            } => (
                "downloading",
                json!({
                    "progress": progress,
                    "bytes_done": bytes_done,
                    "bytes_total": bytes_total,
                }),
            ),
            UpdateCheckStatus::Verifying => ("verifying", Value::Null),
            UpdateCheckStatus::ReadyToInstall { .. } => ("ready-to-install", Value::Null),
            UpdateCheckStatus::Failed(message) => ("failed", json!({ "message": message })),
            UpdateCheckStatus::UpToDate(info) => (
                "up-to-date",
                json!({
                    "latest": info.manifest.latest,
                    "current_version": info.current_version,
                }),
            ),
        };
        json!({
            "status": status,
            "detail": detail,
            "mode": format!("{:?}", self.settings_view_state.app_update_mode).to_ascii_lowercase(),
            "url": self.settings_view_state.app_update_url,
            "github_repo": self.settings_view_state.app_update_github_repo,
            "auto_check": self.settings_view_state.app_update_auto_check,
            "last_check_age_ms": self
                .app_update_last_check
                .map(|at| at.elapsed().as_millis() as u64),
        })
    }

    /// The latest memory-diagnostics sample (working-set / private / tracked
    /// bytes + per-bucket breakdown) and the texture-tracking totals. Turns
    /// texture-leak/memory-growth work into a structured assertion.
    fn agent_gui_memory_value(&self, history: bool, textures: bool) -> Value {
        let sample_to_value = |sample: &MemoryDiagnosticsSample| {
            json!({
                "label": sample.label,
                "age_ms": sample.captured_at.elapsed().as_millis() as u64,
                "working_set_bytes": sample.working_set_bytes,
                "private_bytes": sample.private_bytes,
                "task_manager_memory_bytes": sample.task_manager_memory_bytes,
                "tracked_total_bytes": sample.tracked_total_bytes,
                "untracked_bytes": sample.untracked_bytes,
                "buckets": sample
                    .buckets
                    .iter()
                    .map(|bucket| json!({
                        "label": bucket.label,
                        "bytes": bucket.bytes,
                        "detail": bucket.detail,
                    }))
                    .collect::<Vec<_>>(),
            })
        };
        let icon_texture_bytes: usize = self.tracked_icon_texture_bytes.values().sum();
        let repo_image_texture_bytes: usize = self.tracked_repo_image_texture_bytes.values().sum();
        let mut value = json!({
            "sample_count": self.memory_diagnostics_history.len(),
            "latest": self
                .memory_diagnostics_history
                .back()
                .map(sample_to_value)
                .unwrap_or(Value::Null),
            "icon_texture_bytes": icon_texture_bytes,
            "icon_texture_count": self.tracked_icon_texture_bytes.len(),
            "repo_image_texture_bytes": repo_image_texture_bytes,
            "repo_image_texture_count": self.tracked_repo_image_texture_bytes.len(),
            "app_icon_texture_bytes": self.app_icon_texture_bytes,
            "default_repo_image_texture_bytes": self.default_repo_image_texture_bytes,
        });
        if history && let Value::Object(map) = &mut value {
            map.insert(
                "history".to_string(),
                Value::Array(
                    self.memory_diagnostics_history
                        .iter()
                        .map(sample_to_value)
                        .collect(),
                ),
            );
        }
        if textures && let Value::Object(map) = &mut value {
            let to_rows = |tracked: &std::collections::HashMap<String, usize>| {
                let mut rows: Vec<Value> = tracked
                    .iter()
                    .map(|(key, bytes)| json!({ "key": path_basename(key), "bytes": bytes }))
                    .collect();
                rows.sort_by(|a, b| {
                    b.get("bytes")
                        .and_then(Value::as_u64)
                        .cmp(&a.get("bytes").and_then(Value::as_u64))
                });
                rows
            };
            map.insert(
                "icon_textures".to_string(),
                json!(to_rows(&self.tracked_icon_texture_bytes)),
            );
            map.insert(
                "repo_image_textures".to_string(),
                json!(to_rows(&self.tracked_repo_image_texture_bytes)),
            );
        }
        value
    }

    /// OS-level Arma 3 *player* profiles (`detected_arma3_profiles`) that drive
    /// the `-profiles` launch argument. Distinct from Foxy launch profiles;
    /// directory paths are redacted to basenames.
    fn agent_gui_arma_profiles_value(&self) -> Value {
        let profiles: Vec<Value> = self
            .detected_arma3_profiles
            .iter()
            .map(|profile| {
                json!({
                    "name": profile.name,
                    "is_default": profile.is_default,
                    "folder": profile
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default(),
                    "active": self.detected_active_arma3_profile.as_deref() == Some(profile.name.as_str()),
                })
            })
            .collect();
        json!({
            "active_profile": self.detected_active_arma3_profile,
            "total": profiles.len(),
            "profiles": profiles,
        })
    }

    /// Addon-backup records (`backup_manager_records`): addon, timestamp, size,
    /// and per-addon counts so an agent can assert an update produced a backup
    /// and that retention behaves. Backup file paths are redacted to basenames.
    fn agent_gui_backups_value(&self, contains: Option<&str>, limit: Option<usize>) -> Value {
        let needle = contains.map(|value| value.to_ascii_lowercase());
        let mut per_addon: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut rows: Vec<Value> = Vec::new();
        let mut total_size: u64 = 0;
        let mut matched = 0usize;
        for record in &self.backup_manager_records {
            if let Some(needle) = &needle
                && !record.addon_name.to_ascii_lowercase().contains(needle)
            {
                continue;
            }
            *per_addon.entry(record.addon_name.clone()).or_default() += 1;
            matched += 1;
            total_size = total_size.saturating_add(record.size_bytes);
            if limit.is_none_or(|limit| rows.len() < limit) {
                rows.push(json!({
                    "addon_name": record.addon_name,
                    "folder_name": record.folder_name,
                    "content_hash": record.content_hash,
                    "created_at_unix_secs": record.created_at_unix_secs,
                    "size_bytes": record.size_bytes,
                }));
            }
        }
        let notice = self
            .backup_manager_notice
            .as_ref()
            .map(|notice| json!({ "success": notice.success, "message": notice.message }));
        json!({
            "loaded": self.backup_manager_loaded,
            "total": matched,
            "returned": rows.len(),
            "total_size_bytes": total_size,
            "distinct_addon_count": per_addon.len(),
            "count_per_addon": per_addon,
            "status": self.addon_backup_status.as_ref().map(|status| status.status_text.clone()),
            "notice": notice,
            "backups": rows,
        })
    }
}

impl Foxy {
    // ── drag (#4): advance one event per rendered frame ─────────────────────

    /// Drive parked `drag` gestures: emit the down event, then `steps`
    /// interpolated moves, then the up event - one per frame so egui classifies
    /// the sequence as a real drag.
    fn agent_gui_complete_drags(&mut self, ctx: &egui::Context, runtime: &mut AgentGuiRuntime) {
        let now = Instant::now();
        let mut index = 0;
        while index < runtime.pending_drags.len() {
            let current_frame = ctx.cumulative_frame_nr();
            if current_frame < runtime.pending_drags[index].next_frame {
                if now >= runtime.pending_drags[index].deadline {
                    let drag = runtime.pending_drags.remove(index);
                    let response = AgentGuiResponse::ok(
                        &drag.command,
                        self.agent_gui_view_name(),
                        drag.requested_at,
                        self.agent_gui_snapshot_value(ctx),
                    );
                    let _ = drag.response_tx.send(response);
                    continue;
                }
                ctx.request_repaint_after(Duration::from_millis(16));
                index += 1;
                continue;
            }

            let (from, to, button, steps, phase) = {
                let drag = &runtime.pending_drags[index];
                (drag.from, drag.to, drag.button, drag.steps, drag.phase)
            };
            let mods = Modifiers::default();
            if phase == 0 {
                ctx.input_mut(|input| {
                    input.events.push(Event::PointerMoved(from));
                    input.events.push(Event::PointerButton {
                        pos: from,
                        button,
                        pressed: true,
                        modifiers: mods,
                    });
                });
            } else if phase <= steps {
                let t = phase as f32 / steps as f32;
                let pos = Pos2::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                ctx.input_mut(|input| input.events.push(Event::PointerMoved(pos)));
            } else {
                ctx.input_mut(|input| {
                    input.events.push(Event::PointerMoved(to));
                    input.events.push(Event::PointerButton {
                        pos: to,
                        button,
                        pressed: false,
                        modifiers: mods,
                    });
                });
            }
            ctx.request_repaint();

            // phase > steps means we just emitted the pointer-up: gesture done.
            if phase > steps {
                let drag = runtime.pending_drags.remove(index);
                let response = AgentGuiResponse::ok(
                    &drag.command,
                    self.agent_gui_view_name(),
                    drag.requested_at,
                    self.agent_gui_snapshot_value(ctx),
                );
                let _ = drag.response_tx.send(response);
                continue;
            }
            let drag = &mut runtime.pending_drags[index];
            drag.phase += 1;
            drag.next_frame = current_frame + 1;
            index += 1;
        }
    }

    // ── batch (#2): server-side pipeline ────────────────────────────────────

    /// Begin a `batch`: drive it immediately, parking it for later resumption
    /// only if a step has not yet completed.
    fn agent_gui_start_batch(
        &mut self,
        ctx: &egui::Context,
        runtime: &mut AgentGuiRuntime,
        steps: Vec<AgentGuiCommand>,
        stop_on_error: bool,
        response_tx: Sender<AgentGuiResponse>,
        started_at: Instant,
    ) {
        let total: Duration = steps
            .iter()
            .map(|step| step.timeout())
            .fold(Duration::ZERO, |acc, step| acc.saturating_add(step));
        let deadline = Instant::now()
            .checked_add(
                total
                    .saturating_add(SETTLE_SLACK)
                    .max(DEFAULT_COMMAND_TIMEOUT),
            )
            .unwrap_or_else(Instant::now);
        let mut batch = PendingBatch {
            steps,
            next_index: 0,
            results: Vec::new(),
            stop_on_error,
            all_ok: true,
            response_tx,
            requested_at: started_at,
            deadline,
            step_rx: None,
        };
        match self.drive_batch(ctx, runtime, &mut batch) {
            Some(response) => {
                let _ = batch.response_tx.send(response);
            }
            None => {
                runtime.pending_batches.push(batch);
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
    }

    /// Resume parked batch pipelines whose current step may have completed.
    fn agent_gui_advance_batches(&mut self, ctx: &egui::Context, runtime: &mut AgentGuiRuntime) {
        let mut index = 0;
        while index < runtime.pending_batches.len() {
            let mut batch = runtime.pending_batches.remove(index);
            match self.drive_batch(ctx, runtime, &mut batch) {
                Some(response) => {
                    let _ = batch.response_tx.send(response);
                }
                None => {
                    runtime.pending_batches.insert(index, batch);
                    index += 1;
                }
            }
        }
    }

    /// Run a batch forward until a step parks or all steps are consumed.
    /// Returns the final response when the batch is complete, else `None`.
    fn drive_batch(
        &mut self,
        ctx: &egui::Context,
        runtime: &mut AgentGuiRuntime,
        batch: &mut PendingBatch,
    ) -> Option<AgentGuiResponse> {
        use std::sync::mpsc::TryRecvError;

        // Resolve a parked step first.
        if batch.step_rx.is_some() {
            let recv = batch.step_rx.as_ref().unwrap().try_recv();
            match recv {
                Ok(response) => {
                    batch.step_rx = None;
                    self.absorb_batch_step(batch, response);
                    if !batch.all_ok && batch.stop_on_error {
                        return Some(self.finish_batch(batch));
                    }
                }
                Err(TryRecvError::Empty) => {
                    if Instant::now() >= batch.deadline {
                        batch.all_ok = false;
                        batch.results.push(json!({
                            "ok": false,
                            "errors": [{ "code": "timeout", "message": "batch step timed out" }],
                        }));
                        return Some(self.finish_batch(batch));
                    }
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return None;
                }
                Err(TryRecvError::Disconnected) => {
                    batch.step_rx = None;
                    batch.all_ok = false;
                    batch.results.push(json!({
                        "ok": false,
                        "errors": [{ "code": "disconnected", "message": "batch step channel closed" }],
                    }));
                    if batch.stop_on_error {
                        return Some(self.finish_batch(batch));
                    }
                }
            }
        }

        while batch.next_index < batch.steps.len() {
            let step = batch.steps[batch.next_index].clone();
            batch.next_index += 1;
            if matches!(step, AgentGuiCommand::Batch { .. }) {
                batch.all_ok = false;
                batch.results.push(json!({
                    "ok": false,
                    "command": "batch",
                    "errors": [{ "code": "nested-batch", "message": "nested batch is not supported" }],
                }));
                if batch.stop_on_error {
                    return Some(self.finish_batch(batch));
                }
                continue;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let sub = AgentGuiUiRequest {
                request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
                command: step,
                response_tx: tx,
            };
            self.handle_agent_gui_request(ctx, runtime, sub);
            match rx.try_recv() {
                Ok(response) => {
                    self.absorb_batch_step(batch, response);
                    if !batch.all_ok && batch.stop_on_error {
                        return Some(self.finish_batch(batch));
                    }
                }
                Err(TryRecvError::Empty) => {
                    // Step parked (settle/wait/screenshot/drag): resume later.
                    batch.step_rx = Some(rx);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return None;
                }
                Err(TryRecvError::Disconnected) => {
                    batch.all_ok = false;
                    batch.results.push(json!({
                        "ok": false,
                        "errors": [{ "code": "disconnected", "message": "batch step channel closed" }],
                    }));
                    if batch.stop_on_error {
                        return Some(self.finish_batch(batch));
                    }
                }
            }
        }
        Some(self.finish_batch(batch))
    }

    /// Fold one sub-step response into the batch accumulator. A step is "ok"
    /// when the transport succeeded *and* (for `assert`) its payload `ok` holds.
    fn absorb_batch_step(&self, batch: &mut PendingBatch, response: AgentGuiResponse) {
        let step_ok = response.ok
            && response
                .data
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(true);
        if !step_ok {
            batch.all_ok = false;
        }
        batch
            .results
            .push(serde_json::to_value(&response).unwrap_or(Value::Null));
    }

    fn finish_batch(&self, batch: &PendingBatch) -> AgentGuiResponse {
        AgentGuiResponse::ok(
            &AgentGuiCommand::Batch {
                steps: Vec::new(),
                stop_on_error: batch.stop_on_error,
            },
            self.agent_gui_view_name(),
            batch.requested_at,
            json!({
                "ok": batch.all_ok,
                "total": batch.results.len(),
                "results": batch.results.clone(),
            }),
        )
    }

    // ── diff (#3): structural delta ─────────────────────────────────────────

    /// Compute the delta from a stored baseline to the current observation, and
    /// return the current snapshot so the caller can store it as a new baseline.
    fn agent_gui_diff_value(
        &self,
        ctx: &egui::Context,
        runtime: &AgentGuiRuntime,
        baseline: &str,
    ) -> (Value, AgentGuiSnapshot) {
        let current = self.agent_gui_snapshot(ctx);
        let data = match agent_gui_resolve_baseline(runtime, baseline) {
            Some(base) => agent_gui_snapshot_diff(&base, &current),
            None => json!({
                "baseline": baseline,
                "available": false,
                "note": "no stored baseline yet; this observation is now stored for the next diff",
            }),
        };
        (data, current)
    }

    // ── query (#5): JMESPath over the union state document ───────────────────

    fn agent_gui_query_value(&self, ctx: &egui::Context, expr: &str) -> Result<Value, String> {
        let document = self.agent_gui_state_document(ctx, expr);
        let compiled =
            jmespath::compile(expr).map_err(|e| format!("Invalid JMESPath '{expr}': {e}"))?;
        let variable = jmespath::Variable::from_json(&document.to_string())
            .map_err(|e| format!("Failed to build query document: {e}"))?;
        let result = compiled
            .search(variable)
            .map_err(|e| format!("Query failed: {e}"))?;
        let value = serde_json::to_value(result.as_ref()).unwrap_or(Value::Null);
        Ok(json!({ "expr": expr, "result": value }))
    }

    /// Build the virtual app-state document the `query` evaluates against,
    /// composing the existing structured-state builders. Only the sub-documents
    /// the expression mentions are built (the snapshot is the default context).
    fn agent_gui_state_document(&self, ctx: &egui::Context, expr: &str) -> Value {
        let wants = |key: &str| expr.contains(key);
        let inner =
            |value: Value, key: &str| value.get(key).cloned().unwrap_or(Value::Array(vec![]));
        let mut doc = serde_json::Map::new();

        const SECTIONS: &[&str] = &[
            "settings",
            "progress",
            "repositories",
            "spaces",
            "profiles",
            "addons",
            "missions",
            "inventory",
            "toasts",
            "filters",
            "pending_updates",
            "download_summary",
            "app_update",
            "memory",
            "backups",
            "arma_profiles",
        ];
        let any_section = SECTIONS.iter().any(|key| wants(key));
        if wants("snapshot") || !any_section {
            doc.insert("snapshot".to_string(), self.agent_gui_snapshot_value(ctx));
        }
        if wants("settings") {
            doc.insert("settings".to_string(), self.agent_gui_settings_value());
        }
        if wants("progress") {
            doc.insert("progress".to_string(), self.agent_gui_progress_value());
        }
        if wants("repositories") {
            doc.insert(
                "repositories".to_string(),
                inner(
                    self.agent_gui_repositories_value(None, None),
                    "repositories",
                ),
            );
        }
        if wants("spaces") {
            doc.insert(
                "spaces".to_string(),
                inner(self.agent_gui_spaces_value(None, None), "spaces"),
            );
        }
        if wants("missions") {
            doc.insert(
                "missions".to_string(),
                inner(self.agent_gui_missions_value(None, None), "missions"),
            );
        }
        if wants("inventory") {
            doc.insert(
                "inventory".to_string(),
                inner(
                    self.agent_gui_inventory_value(None, None, None, None),
                    "addons",
                ),
            );
        }
        if wants("toasts") {
            doc.insert("toasts".to_string(), self.agent_gui_toasts_value());
        }
        if wants("filters") {
            doc.insert("filters".to_string(), self.agent_gui_filters_value());
        }
        if wants("pending_updates") {
            doc.insert(
                "pending_updates".to_string(),
                self.agent_gui_pending_updates_value(None, None, None, false),
            );
        }
        if wants("download_summary") {
            doc.insert(
                "download_summary".to_string(),
                self.agent_gui_download_summary_value(false),
            );
        }
        if wants("app_update") {
            doc.insert("app_update".to_string(), self.agent_gui_app_update_value());
        }
        if wants("memory") {
            doc.insert(
                "memory".to_string(),
                self.agent_gui_memory_value(false, false),
            );
        }
        if wants("backups") {
            doc.insert(
                "backups".to_string(),
                inner(self.agent_gui_backups_value(None, None), "backups"),
            );
        }
        if wants("arma_profiles") {
            doc.insert(
                "arma_profiles".to_string(),
                inner(self.agent_gui_arma_profiles_value(), "profiles"),
            );
        }
        if wants("addons")
            && let Ok(value) = self.agent_gui_addons_value(None, None, None, false, None)
        {
            doc.insert("addons".to_string(), inner(value, "addons"));
        }
        if wants("profiles")
            && let Ok(value) = self.agent_gui_profiles_value(None, None, None)
        {
            doc.insert("profiles".to_string(), inner(value, "profiles"));
        }
        Value::Object(doc)
    }

    // ── checkpoint / restore (#6): UI-state save & rollback ──────────────────

    /// Snapshot the serializable UI-state subset for `checkpoint`. UI state
    /// only - never core/DB/disk state.
    fn agent_gui_capture_checkpoint(&self, _ctx: &egui::Context) -> Value {
        json!({
            "current_view": self.agent_gui_view_name(),
            "selected_repository": self.repository_view_state.selected_repository,
            "selected_repository_for_settings": self.selected_repository_for_settings,
            "repository_settings_tab": self.current_repository_settings_tab.as_str(),
            "selected_repository_space_id": self.selected_repository_space_id,
            "show_add_repository_modal": self.show_add_repository_modal,
            "filters": self.agent_gui_filters_value(),
            "settings_view_state": self.agent_gui_settings_value(),
        })
    }

    /// Write a captured checkpoint back onto the live UI state.
    fn agent_gui_restore_checkpoint(&mut self, ctx: &egui::Context, state: &Value) {
        if let Some(view) = state.get("current_view").and_then(Value::as_str)
            && let Some(parsed) = parse_agent_gui_view(view)
        {
            self.current_view = parsed;
        }
        if let Some(field) = state.get("selected_repository") {
            self.repository_view_state.selected_repository =
                field.as_u64().map(|index| index as usize);
        }
        if let Some(field) = state.get("selected_repository_for_settings") {
            self.selected_repository_for_settings = field.as_u64().map(|index| index as usize);
        }
        if let Some(tab) = state.get("repository_settings_tab").and_then(Value::as_str)
            && let Some(parsed) = parse_agent_gui_repository_settings_tab(tab)
        {
            self.current_repository_settings_tab = parsed;
        }
        if let Some(field) = state.get("selected_repository_space_id") {
            self.selected_repository_space_id = field.as_str().map(str::to_string);
        }
        if let Some(open) = state
            .get("show_add_repository_modal")
            .and_then(Value::as_bool)
        {
            self.show_add_repository_modal = open;
        }
        if let Some(filters) = state.get("filters") {
            self.agent_gui_restore_filters(filters);
        }
        if let Some(settings) = state.get("settings_view_state")
            && let Ok(parsed) = serde_json::from_value(settings.clone())
        {
            self.settings_view_state = parsed;
            self.mark_settings_dirty();
        }
        ctx.request_repaint();
    }

    /// Write the list-filter subset of a checkpoint back onto live state.
    fn agent_gui_restore_filters(&mut self, filters: &Value) {
        let string =
            |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(String::from);
        let boolean = |value: &Value, key: &str| value.get(key).and_then(Value::as_bool);
        if let Some(value) = string(filters, "addons_filter") {
            self.addons_filter = value;
        }
        if let Some(value) = string(filters, "optional_addons_filter") {
            self.optional_addons_filter = value;
        }
        if let Some(value) = string(filters, "external_addons_filter") {
            self.external_addons_filter = value;
        }
        if let Some(value) = string(filters, "external_addons_origin_filter") {
            self.external_addons_origin_filter = value;
        }
        if let Some(value) = boolean(filters, "external_addons_group_by_origin") {
            self.external_addons_group_by_origin = value;
        }
        if let Some(value) = string(filters, "addon_state_filter") {
            self.addon_state_filter = value;
        }
        if let Some(value) = boolean(filters, "addon_favorites_only_filter") {
            self.addon_favorites_only_filter = value;
        }
        if let Some(value) = boolean(filters, "addon_client_side_only_filter") {
            self.addon_client_side_only_filter = value;
        }
        if let Some(value) = string(filters, "editor_mission_search") {
            self.editor_mission_search = value;
        }
        if let Some(value) = string(filters, "editor_mission_terrain_filter") {
            self.editor_mission_terrain_filter = value;
        }
        if let Some(value) = boolean(filters, "editor_mission_show_folders") {
            self.editor_mission_show_folders = value;
        }
        if let Some(value) = string(filters, "repository_space_detail_filter") {
            self.repository_space_detail_filter = value;
        }
        if let Some(value) = string(filters, "backup_manager_filter") {
            self.backup_manager_filter = value;
        }
    }

    // ── element (#7): deep single-node introspection ─────────────────────────

    fn agent_gui_element_value(
        &self,
        ctx: &egui::Context,
        id: Option<&str>,
        x: Option<f32>,
        y: Option<f32>,
    ) -> Result<Value, String> {
        let nodes = self.agent_gui_nodes(ctx);
        let pointer = ctx.pointer_latest_pos();
        let focused = agent_gui_focused_name(ctx);

        let (node, sibling_index) = if let Some(id) = id {
            let normalized = normalize_selector(id);
            nodes
                .iter()
                .enumerate()
                .find(|(_, node)| normalize_selector(&node.id) == normalized)
                .map(|(index, node)| (node.clone(), index))
                .ok_or_else(|| format!("No known node matched id '{id}'"))?
        } else if let (Some(x), Some(y)) = (x, y) {
            let point = Pos2::new(x, y);
            let area = |node: &AgentGuiNode| {
                node.rect
                    .as_ref()
                    .map(|rect| rect.w * rect.h)
                    .unwrap_or(f32::MAX)
            };
            nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.rect.as_ref().is_some_and(|rect| {
                        point.x >= rect.x
                            && point.x <= rect.x + rect.w
                            && point.y >= rect.y
                            && point.y <= rect.y + rect.h
                    })
                })
                .min_by(|(_, a), (_, b)| {
                    area(a)
                        .partial_cmp(&area(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, node)| (node.clone(), index))
                .ok_or_else(|| format!("No known node under ({x}, {y})"))?
        } else {
            return Err("Provide --id, or both --x and --y".to_string());
        };

        let hovered = pointer
            .zip(node.rect.as_ref())
            .map(|(pos, rect)| {
                pos.x >= rect.x
                    && pos.x <= rect.x + rect.w
                    && pos.y >= rect.y
                    && pos.y <= rect.y + rect.h
            })
            .unwrap_or(false);
        let is_focused = focused.as_deref() == Some(node.id.as_str()) || node.focused;
        Ok(json!({
            "found": true,
            "id": node.id,
            "role": node.role,
            "text": node.text,
            "enabled": node.enabled,
            "focused": is_focused,
            "hovered": hovered,
            "rect": node.rect,
            "parent_id": Value::Null,
            "child_ids": Vec::<String>::new(),
            "sibling_index": sibling_index,
            "has_click": node.role == "button",
            "has_drag": false,
            "has_scroll": false,
            "tooltip": Value::Null,
            "maps_to_action": agent_gui_node_action(&node.id),
        }))
    }

    // ── events (#8): derive transition events each frame ─────────────────────

    /// Record view/modal/focus/toast/download transitions by diffing the frame
    /// just observed against the previously recorded state.
    fn agent_gui_record_state_events(
        &mut self,
        ctx: &egui::Context,
        runtime: &mut AgentGuiRuntime,
    ) {
        let view = self.agent_gui_view_name().to_string();
        if runtime.prev_view.as_deref() != Some(view.as_str()) {
            if runtime.prev_view.is_some() {
                runtime.record_event(
                    "view-change",
                    json!({ "from": runtime.prev_view, "to": view }),
                );
            }
            runtime.prev_view = Some(view);
        }

        let modals: Vec<String> = self
            .agent_gui_modal_names()
            .into_iter()
            .map(String::from)
            .collect();
        let prev_modals = std::mem::take(&mut runtime.prev_modals);
        for modal in &modals {
            if !prev_modals.contains(modal) {
                runtime.record_event("modal-open", json!({ "modal": modal }));
            }
        }
        for modal in &prev_modals {
            if !modals.contains(modal) {
                runtime.record_event("modal-close", json!({ "modal": modal }));
            }
        }
        runtime.prev_modals = modals;

        let focused = agent_gui_focused_name(ctx);
        if runtime.prev_focused != focused {
            runtime.record_event(
                "focus-change",
                json!({ "from": runtime.prev_focused, "to": focused }),
            );
            runtime.prev_focused = focused;
        }

        let toast = self
            .agent_gui_current_toast()
            .map(|toast| toast.message.clone());
        if runtime.prev_toast != toast {
            if let Some(message) = &toast {
                runtime.record_event("toast-shown", json!({ "message": message }));
            }
            runtime.prev_toast = toast;
        }

        let download_active = self.download_progress.is_some();
        if download_active != runtime.prev_download_active {
            runtime.record_event("download-state", json!({ "active": download_active }));
            runtime.prev_download_active = download_active;
        }
        if self.download_finished != runtime.prev_download_finished {
            if self.download_finished {
                runtime.record_event("download-state", json!({ "finished": true }));
            }
            runtime.prev_download_finished = self.download_finished;
        }
    }

    // ── invoke (#1): semantic app-action registry ────────────────────────────

    /// Resolve the `repo-index` param (or the selected repository) and validate
    /// it against the configured list.
    fn agent_gui_resolve_repo_index(&self, params: &Value) -> Result<usize, (String, String)> {
        let index = params
            .get("repo-index")
            .or_else(|| params.get("repo_index"))
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            .or(self.repository_view_state.selected_repository)
            .ok_or_else(|| {
                (
                    "invalid-params".to_string(),
                    "Provide repo-index or select a repository first".to_string(),
                )
            })?;
        if index >= self.repository_view_state.repositories.len() {
            return Err((
                "invalid-params".to_string(),
                format!("Repository index {index} is out of range"),
            ));
        }
        Ok(index)
    }

    /// Run one named semantic action. Errors are `(code, message)`.
    fn agent_gui_invoke(
        &mut self,
        ctx: &egui::Context,
        action: &str,
        params: &Value,
        allow_destructive: bool,
    ) -> Result<Value, (String, String)> {
        let name = normalize_selector(action);
        let meta = AGENT_ACTIONS
            .iter()
            .find(|candidate| candidate.name == name)
            .ok_or_else(|| {
                (
                    "unknown-action".to_string(),
                    format!("Unknown action '{action}'. Use `invoke --list-actions` to enumerate."),
                )
            })?;
        if meta.destructive && !allow_destructive {
            return Err((
                "destructive".to_string(),
                format!("Action '{name}' mutates core/disk state; re-run with --allow-destructive"),
            ));
        }
        match name.as_str() {
            "open-settings" => {
                if self.current_view != FoxyView::Settings {
                    self.open_settings_view();
                }
                if let Some(tab) = params.get("tab").and_then(Value::as_str) {
                    self.settings_view_state.current_tab = parse_agent_gui_settings_tab(tab)
                        .ok_or_else(|| {
                            (
                                "invalid-params".to_string(),
                                format!("Unsupported settings tab '{tab}'"),
                            )
                        })?;
                }
            }
            "open-repository-list" => {
                self.current_view = FoxyView::RepositoryList;
                self.last_view = FoxyView::None;
            }
            "open-changelog" => self.open_reference_view(FoxyView::Changelog),
            "open-about" => self.open_reference_view(FoxyView::About),
            "open-help" => self.open_reference_view(FoxyView::Help),
            "open-app-update" => self.open_reference_view(FoxyView::AppUpdate),
            "open-add-repository-modal" => self.show_add_repository_modal = true,
            "close-modals" => self.agent_gui_close_modals(),
            "toggle-activity-log" => self.set_activity_log_visibility(
                ctx,
                !self.settings_view_state.show_activity_log,
                "agent GUI driver",
            ),
            "select-repository" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.repository_view_state.selected_repository = Some(index);
                self.selected_repository_for_settings = Some(index);
            }
            "apply-profile" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                let profile = params
                    .get("profile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        (
                            "invalid-params".to_string(),
                            "Provide a profile name (--profile)".to_string(),
                        )
                    })?
                    .to_string();
                self.agent_gui_apply_profile(index, &profile)?;
            }
            "start-sync" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.start_core_sync(index, api::SyncMode::Download);
            }
            "recheck-repo" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.start_core_sync(index, api::SyncMode::RecheckOnly);
            }
            "recheck-integrity" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.start_core_sync(index, api::SyncMode::RecheckIntegrity);
            }
            "force-redownload" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.force_redownload_repository(index);
            }
            "pause-download" => self.set_download_paused(true),
            "resume-download" => self.set_download_paused(false),
            "cancel-download" => self.cancel_sync(),
            "launch-game" => {
                let index = self.agent_gui_resolve_repo_index(params)?;
                self.agent_gui_launch_repository(ctx, index)?;
            }
            other => {
                return Err((
                    "unknown-action".to_string(),
                    format!("Action '{other}' is registered but not implemented"),
                ));
            }
        }
        ctx.request_repaint();
        Ok(json!({ "action": name, "ran": true, "destructive": meta.destructive }))
    }

    /// Clear the simple boolean modal/window flags (non-destructive). Used by
    /// the `close-modals` action so exploration can back out of an open dialog.
    fn agent_gui_close_modals(&mut self) {
        self.update_modal_open = false;
        self.show_add_repository_modal = false;
        self.show_add_profile_window = false;
        self.show_rename_profile_window = false;
        self.show_memory_diagnostics_window = false;
    }

    /// Select and apply a launch profile to a repository (the same mutation the
    /// profile dropdown performs).
    fn agent_gui_apply_profile(
        &mut self,
        index: usize,
        profile_name: &str,
    ) -> Result<(), (String, String)> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(index)
            .ok_or_else(|| {
                (
                    "not-found".to_string(),
                    format!("Repository index {index} is out of range"),
                )
            })?;
        let profile = repo
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .cloned()
            .ok_or_else(|| {
                (
                    "not-found".to_string(),
                    format!("No profile named '{profile_name}'"),
                )
            })?;
        repo.selected_profile = Some(profile.name.clone());
        Self::apply_profile_to_repository(repo, &profile);
        Ok(())
    }

    /// Launch Arma 3 for a repository through the same preflight path the Launch
    /// button uses (no server = a regular launch).
    fn agent_gui_launch_repository(
        &mut self,
        ctx: &egui::Context,
        index: usize,
    ) -> Result<(), (String, String)> {
        let repo = self
            .repository_view_state
            .repositories
            .get(index)
            .cloned()
            .ok_or_else(|| {
                (
                    "not-found".to_string(),
                    format!("Repository index {index} is out of range"),
                )
            })?;
        let effective = Self::build_effective_repository_snapshot(&repo);
        let repo_name = repo.name.clone();
        self.present_launch_preflight(ctx, &effective, &repo_name);
        Ok(())
    }
}

/// One registered semantic action surfaced by `invoke --list-actions`.
struct AgentAction {
    name: &'static str,
    destructive: bool,
    params: &'static str,
    summary: &'static str,
}

/// The semantic-action registry. Non-destructive intents are always runnable;
/// destructive ones require `allow_destructive` (and keep Foxy's normal
/// confirmation semantics - e.g. `launch-game` still routes through preflight).
const AGENT_ACTIONS: &[AgentAction] = &[
    AgentAction {
        name: "open-settings",
        destructive: false,
        params: "tab",
        summary: "Open the Settings view, optionally to a named settings tab",
    },
    AgentAction {
        name: "open-repository-list",
        destructive: false,
        params: "",
        summary: "Open the repository list",
    },
    AgentAction {
        name: "open-changelog",
        destructive: false,
        params: "",
        summary: "Open the changelog view",
    },
    AgentAction {
        name: "open-about",
        destructive: false,
        params: "",
        summary: "Open the about view",
    },
    AgentAction {
        name: "open-help",
        destructive: false,
        params: "",
        summary: "Open the help view",
    },
    AgentAction {
        name: "open-app-update",
        destructive: false,
        params: "",
        summary: "Open the app-update view",
    },
    AgentAction {
        name: "open-add-repository-modal",
        destructive: false,
        params: "",
        summary: "Open the add-repository modal",
    },
    AgentAction {
        name: "close-modals",
        destructive: false,
        params: "",
        summary: "Close simple modal/window flags",
    },
    AgentAction {
        name: "toggle-activity-log",
        destructive: false,
        params: "",
        summary: "Toggle the footer activity log",
    },
    AgentAction {
        name: "select-repository",
        destructive: false,
        params: "repo-index",
        summary: "Select/highlight a repository",
    },
    AgentAction {
        name: "apply-profile",
        destructive: false,
        params: "repo-index, profile",
        summary: "Select and apply a launch profile",
    },
    AgentAction {
        name: "start-sync",
        destructive: true,
        params: "repo-index",
        summary: "Start a download sync for a repository",
    },
    AgentAction {
        name: "recheck-repo",
        destructive: true,
        params: "repo-index",
        summary: "Recheck a repository (remote refresh)",
    },
    AgentAction {
        name: "recheck-integrity",
        destructive: true,
        params: "repo-index",
        summary: "Full integrity recheck of a repository",
    },
    AgentAction {
        name: "force-redownload",
        destructive: true,
        params: "repo-index",
        summary: "Force a full redownload of a repository",
    },
    AgentAction {
        name: "pause-download",
        destructive: false,
        params: "",
        summary: "Pause the active download",
    },
    AgentAction {
        name: "resume-download",
        destructive: false,
        params: "",
        summary: "Resume the paused download",
    },
    AgentAction {
        name: "cancel-download",
        destructive: false,
        params: "",
        summary: "Cancel the active sync/download",
    },
    AgentAction {
        name: "launch-game",
        destructive: true,
        params: "repo-index",
        summary: "Launch Arma 3 for a repository (via preflight)",
    },
];

/// The `invoke --list-actions` payload.
fn agent_gui_list_actions_value() -> Value {
    let actions: Vec<Value> = AGENT_ACTIONS
        .iter()
        .map(|action| {
            json!({
                "name": action.name,
                "destructive": action.destructive,
                "params": action.params,
                "summary": action.summary,
            })
        })
        .collect();
    json!({ "actions": actions })
}

/// Map a known semantic node id to its `invoke` action name, when one exists
/// (used by `element.maps_to_action`).
fn agent_gui_node_action(id: &str) -> Value {
    let action = match normalize_selector(id).as_str() {
        "header.settings" => Some("open-settings"),
        "footer.changelog" => Some("open-changelog"),
        "footer.about" => Some("open-about"),
        "footer.help" => Some("open-help"),
        _ => None,
    };
    action.map(|name| json!(name)).unwrap_or(Value::Null)
}

/// Resolve a `diff` baseline selector (`last` or `frame:<n>`) to a stored
/// observation. `frame:<n>` prefers an exact frame match, else the most recent
/// observation at or before that frame.
fn agent_gui_resolve_baseline(
    runtime: &AgentGuiRuntime,
    baseline: &str,
) -> Option<AgentGuiSnapshot> {
    let baseline = baseline.trim();
    if baseline.eq_ignore_ascii_case("last") {
        return runtime.diff_baselines.back().cloned();
    }
    if let Some(rest) = baseline.strip_prefix("frame:")
        && let Ok(target) = rest.trim().parse::<u64>()
    {
        return runtime
            .diff_baselines
            .iter()
            .rev()
            .find(|snapshot| snapshot.frame == target)
            .cloned()
            .or_else(|| {
                runtime
                    .diff_baselines
                    .iter()
                    .rev()
                    .find(|snapshot| snapshot.frame <= target)
                    .cloned()
            });
    }
    None
}

/// Field-level delta between two observations: node id set differences, text
/// set differences, and changed scalar/array fields (high-noise fields like
/// `fps`/`frame`/`pointer` are excluded).
fn agent_gui_snapshot_diff(base: &AgentGuiSnapshot, current: &AgentGuiSnapshot) -> Value {
    let base_ids: BTreeSet<&str> = base.nodes.iter().map(|node| node.id.as_str()).collect();
    let current_ids: BTreeSet<&str> = current.nodes.iter().map(|node| node.id.as_str()).collect();
    let added_nodes: Vec<Value> = current
        .nodes
        .iter()
        .filter(|node| !base_ids.contains(node.id.as_str()))
        .map(|node| json!({ "id": node.id, "role": node.role, "text": node.text }))
        .collect();
    let removed_nodes: Vec<Value> = base
        .nodes
        .iter()
        .filter(|node| !current_ids.contains(node.id.as_str()))
        .map(|node| json!({ "id": node.id, "role": node.role, "text": node.text }))
        .collect();

    let base_texts: BTreeSet<&str> = base.texts.iter().map(String::as_str).collect();
    let current_texts: BTreeSet<&str> = current.texts.iter().map(String::as_str).collect();
    let text_added: Vec<&str> = current_texts.difference(&base_texts).copied().collect();
    let text_removed: Vec<&str> = base_texts.difference(&current_texts).copied().collect();

    const SKIP: &[&str] = &[
        "nodes",
        "texts",
        "fps",
        "frame",
        "cumulative_pass_nr",
        "pointer",
        "content_rect",
        "startup_frame_rendered",
    ];
    let base_value = serde_json::to_value(base).unwrap_or_default();
    let current_value = serde_json::to_value(current).unwrap_or_default();
    let mut changed = serde_json::Map::new();
    if let (Value::Object(base_map), Value::Object(current_map)) = (&base_value, &current_value) {
        let keys: BTreeSet<&String> = base_map.keys().chain(current_map.keys()).collect();
        for key in keys {
            if SKIP.contains(&key.as_str()) {
                continue;
            }
            let before = base_map.get(key).unwrap_or(&Value::Null);
            let after = current_map.get(key).unwrap_or(&Value::Null);
            if before != after {
                changed.insert(key.clone(), json!({ "from": before, "to": after }));
            }
        }
    }

    json!({
        "baseline_frame": base.frame,
        "current_frame": current.frame,
        "added_nodes": added_nodes,
        "removed_nodes": removed_nodes,
        "changed_fields": Value::Object(changed),
        "text_added": text_added,
        "text_removed": text_removed,
    })
}

/// The `events` payload: filter by kind/since and tail to `limit`.
fn agent_gui_events_value(
    runtime: &AgentGuiRuntime,
    kinds: Option<&[String]>,
    since: Option<u64>,
    limit: Option<usize>,
) -> Value {
    let kind_filter: Option<BTreeSet<String>> =
        kinds.map(|kinds| kinds.iter().map(|kind| normalize_selector(kind)).collect());
    let since = since.unwrap_or(0);
    let mut entries: Vec<&AgentUiEvent> = runtime
        .events
        .iter()
        .filter(|event| event.generation > since)
        .filter(|event| {
            kind_filter
                .as_ref()
                .is_none_or(|set| set.contains(&event.kind))
        })
        .collect();
    if let Some(limit) = limit {
        let skip = entries.len().saturating_sub(limit);
        entries.drain(..skip);
    }
    json!({
        "generation": runtime.event_generation,
        "count": entries.len(),
        "events": serde_json::to_value(&entries).unwrap_or_else(|_| json!([])),
    })
}

/// The `clock` command: advance/freeze/resume/status the virtual UI clock.
fn agent_gui_clock(ctx: &egui::Context, action: &str, ms: Option<u64>) -> Result<Value, String> {
    use crate::ui::app::agent_support;
    let normalized = normalize_selector(action);
    match normalized.as_str() {
        "advance" => {
            let ms = ms.ok_or_else(|| "clock advance requires --ms <n>".to_string())?;
            agent_support::clock_advance(ms);
        }
        "freeze" => agent_support::clock_freeze(),
        "resume" => agent_support::clock_resume(),
        "status" => {}
        other => {
            return Err(format!(
                "Unsupported clock action '{other}' (use advance, freeze, resume, or status)"
            ));
        }
    }
    ctx.request_repaint();
    let (offset_ms, frozen) = agent_support::clock_state();
    Ok(json!({ "action": normalized, "offset_ms": offset_ms, "frozen": frozen }))
}

/// The `dialog` command: queue a response for the next native picker, report
/// picker state, or clear the queued response. Paths are redacted to basenames.
fn agent_gui_dialog(action: &str, path: Option<&Path>, cancel: bool) -> Result<Value, String> {
    use crate::ui::app::agent_support::{self, QueuedDialog};
    match normalize_selector(action).as_str() {
        "expect" => {
            if cancel {
                agent_support::queue_dialog_response(QueuedDialog::Cancel);
                Ok(json!({ "expecting": "cancel", "queued": true }))
            } else if let Some(path) = path {
                agent_support::queue_dialog_response(QueuedDialog::Path(path.to_path_buf()));
                Ok(json!({
                    "expecting": "path",
                    "path": path_basename(&path.to_string_lossy()),
                    "queued": true,
                }))
            } else {
                Err("dialog expect requires --path <p> or --cancel".to_string())
            }
        }
        "clear" => {
            agent_support::clear_dialog_response();
            Ok(json!({ "cleared": true }))
        }
        "pending" => {
            let queued = agent_support::dialog_queued().map(|queued| match queued {
                QueuedDialog::Path(path) => {
                    json!({ "kind": "path", "path": path_basename(path.to_string_lossy().as_ref()) })
                }
                QueuedDialog::Cancel => json!({ "kind": "cancel" }),
            });
            Ok(json!({
                "dialog_open": agent_support::dialog_open(),
                "queued": queued,
                "intercepted_count": agent_support::dialog_intercepted_count(),
            }))
        }
        other => Err(format!(
            "Unsupported dialog action '{other}' (use expect, pending, or clear)"
        )),
    }
}

fn write_color_image_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    image::save_buffer_with_format(
        path,
        image.as_raw(),
        image.size[0] as u32,
        image.size[1] as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("Failed to write screenshot {}: {}", path.display(), e))
}

fn normalize_selector(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Stable agent egui ids for widgets the `focus` command can reach. Each maps a
/// friendly target name to the absolute `egui::Id` the widget is created with
/// (via `.id(...)` at the widget site), so `request_focus` lands deterministically
/// and `snapshot.focused` can report the friendly name back.
const AGENT_FOCUS_TARGETS: &[(&str, &str)] =
    &[("add-repository-input", "agent.add-repository-input")];

/// Resolve a friendly focus target name to its absolute egui id.
fn agent_gui_focus_target_id(name: &str) -> Option<egui::Id> {
    AGENT_FOCUS_TARGETS
        .iter()
        .find(|(target, _)| *target == name)
        .map(|(_, id)| egui::Id::new(id))
}

/// The registered focus target names, for error messages.
fn agent_gui_focus_target_names() -> Vec<&'static str> {
    AGENT_FOCUS_TARGETS.iter().map(|(name, _)| *name).collect()
}

/// The currently keyboard-focused widget as a friendly name when it is a
/// registered agent target, else the raw egui id, else `None`.
fn agent_gui_focused_name(ctx: &egui::Context) -> Option<String> {
    let focused = ctx.memory(|memory| memory.focused())?;
    for (name, id) in AGENT_FOCUS_TARGETS {
        if egui::Id::new(id) == focused {
            return Some((*name).to_string());
        }
    }
    Some(format!("{focused:?}"))
}

/// Keep only the requested top-level keys of a JSON object (server-side
/// `--fields` projection). Non-objects and unknown keys are passed through /
/// dropped without error so a stale field name never wedges a poll loop.
fn project_object_fields(value: Value, fields: &[String]) -> Value {
    let Value::Object(map) = value else {
        return value;
    };
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(found) = map.get(field) {
            projected.insert(field.clone(), found.clone());
        }
    }
    Value::Object(projected)
}

/// Look up a dotted path (`a.b.0.c`) inside a JSON value via JSON Pointer.
fn json_pointer_lookup(value: &Value, dotted: &str) -> Option<Value> {
    if dotted.is_empty() {
        return Some(value.clone());
    }
    let pointer = format!("/{}", dotted.split('.').collect::<Vec<_>>().join("/"));
    value.pointer(&pointer).cloned()
}

/// Stringify a JSON scalar for assertion comparison (strings without quotes).
fn json_value_to_plain_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// The final path component of an on-disk path, used to redact absolute user
/// paths to a basename in harness responses.
fn path_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

/// Sidecar JSON path for an annotated screenshot (`<output>.nodes.json`).
fn annotation_sidecar_path(output: &Path) -> PathBuf {
    let mut name = output
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".nodes.json");
    output.with_file_name(name)
}

/// Write the rect→id sidecar for an annotated screenshot. Rects are in egui
/// logical points; multiply by `scale_factor` for physical pixels.
fn write_annotation_sidecar(
    output: &Path,
    nodes: &[AgentGuiNode],
    pointer: Option<Pos2>,
    scale_factor: f32,
) -> Result<(), String> {
    let path = annotation_sidecar_path(output);
    let payload = json!({
        "scale_factor": scale_factor,
        "pointer": pointer.map(|pos| json!({ "x": pos.x, "y": pos.y })),
        "nodes": nodes
            .iter()
            .filter_map(|node| {
                node.rect.as_ref().map(|rect| json!({
                    "id": node.id,
                    "role": node.role,
                    "rect": { "x": rect.x, "y": rect.y, "w": rect.w, "h": rect.h },
                }))
            })
            .collect::<Vec<_>>(),
    });
    let serialized = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize annotation: {e}"))?;
    fs::write(&path, serialized).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

/// Draw each node's rect outline (and the pointer crosshair) onto a screenshot
/// so a misfiring coordinate click can be debugged from one artifact. Node
/// rects are logical points; scale to physical pixels with `scale_factor`.
fn annotate_color_image(
    image: &mut egui::ColorImage,
    nodes: &[AgentGuiNode],
    pointer: Option<Pos2>,
    scale_factor: f32,
) {
    let outline = egui::Color32::from_rgb(255, 64, 64);
    let crosshair = egui::Color32::from_rgb(64, 255, 64);
    let width = image.size[0];
    let height = image.size[1];
    let mut put = |x: i64, y: i64, color: egui::Color32| {
        if x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height {
            image.pixels[(y as usize) * width + (x as usize)] = color;
        }
    };
    for node in nodes {
        let Some(rect) = node.rect.as_ref() else {
            continue;
        };
        let left = (rect.x * scale_factor).round() as i64;
        let top = (rect.y * scale_factor).round() as i64;
        let right = ((rect.x + rect.w) * scale_factor).round() as i64;
        let bottom = ((rect.y + rect.h) * scale_factor).round() as i64;
        for x in left..=right {
            put(x, top, outline);
            put(x, bottom, outline);
        }
        for y in top..=bottom {
            put(left, y, outline);
            put(right, y, outline);
        }
    }
    if let Some(pos) = pointer {
        let px = (pos.x * scale_factor).round() as i64;
        let py = (pos.y * scale_factor).round() as i64;
        for d in -6..=6 {
            put(px + d, py, crosshair);
            put(px, py + d, crosshair);
        }
    }
}

/// Parse the booleans accepted by `set-setting`. Permissive on purpose so an
/// agent can pass any of the obvious spellings.
fn parse_agent_gui_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "y" => Some(true),
        "false" | "0" | "off" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn toast_kind_name(kind: crate::ui::app::UiToastKind) -> &'static str {
    match kind {
        crate::ui::app::UiToastKind::Success => "success",
        crate::ui::app::UiToastKind::Error => "error",
    }
}

fn parse_pointer_button(name: Option<&str>) -> Result<PointerButton, String> {
    match name.map(normalize_selector).as_deref() {
        None | Some("primary") | Some("left") => Ok(PointerButton::Primary),
        Some("secondary") | Some("right") => Ok(PointerButton::Secondary),
        Some("middle") => Ok(PointerButton::Middle),
        Some(other) => Err(format!(
            "Unsupported mouse button '{other}' (use primary, secondary, or middle)"
        )),
    }
}

/// Read the in-memory activity-log buffer (the same data behind the footer
/// activity log) so agents can assert on app output and egui warnings without
/// tailing the redirected stdout file.
///
/// `since_generation` enables cheap incremental tailing: the log generation is a
/// monotonic counter incremented once per appended entry, so the most recent
/// `generation - since` entries are the ones added since the last read.
fn agent_gui_logs(
    level: Option<&str>,
    contains: Option<&str>,
    limit: Option<usize>,
    since_generation: Option<u64>,
) -> Value {
    let generation = api::activity_log_generation();
    let mut entries = api::activity_log_snapshot();

    if let Some(since) = since_generation {
        let new_count = generation.saturating_sub(since) as usize;
        let skip = entries.len().saturating_sub(new_count);
        entries.drain(..skip);
    }
    if let Some(level) = level {
        entries.retain(|entry| entry.level.eq_ignore_ascii_case(level));
    }
    if let Some(contains) = contains {
        let needle = contains.to_ascii_lowercase();
        entries.retain(|entry| {
            entry.message.to_ascii_lowercase().contains(&needle)
                || entry.source.to_ascii_lowercase().contains(&needle)
        });
    }
    if let Some(limit) = limit {
        let skip = entries.len().saturating_sub(limit);
        entries.drain(..skip);
    }

    json!({
        "generation": generation,
        "count": entries.len(),
        "entries": entries,
    })
}

pub fn parse_agent_gui_view(view: &str) -> Option<FoxyView> {
    match normalize_selector(view).as_str() {
        "repository-list" | "repositories" | "repo-list" => Some(FoxyView::RepositoryList),
        "settings" => Some(FoxyView::Settings),
        "repository-settings" | "repo-settings" => Some(FoxyView::RepositorySettings),
        "repository-space-settings" | "space-settings" => Some(FoxyView::RepositorySpaceSettings),
        "help" => Some(FoxyView::Help),
        "changelog" => Some(FoxyView::Changelog),
        "about" => Some(FoxyView::About),
        "app-update" | "update" => Some(FoxyView::AppUpdate),
        "version-browser" | "versions" => Some(FoxyView::VersionBrowser),
        "swifty-migration" | "migration" => Some(FoxyView::SwiftyMigration),
        "game-spaces" | "games" => Some(FoxyView::GameSpaces),
        "game-space-settings" | "game-settings" => Some(FoxyView::GameSpaceSettings),
        "none" => Some(FoxyView::None),
        _ => None,
    }
}

pub fn parse_agent_gui_repository_settings_tab(tab: &str) -> Option<RepositorySettingsTab> {
    match normalize_selector(tab).as_str() {
        "configuration" | "config" => Some(RepositorySettingsTab::Configuration),
        "addons" | "addon" => Some(RepositorySettingsTab::Addons),
        "optional-addons" | "optional" => Some(RepositorySettingsTab::OptionalAddons),
        "external-addons" | "external" => Some(RepositorySettingsTab::ExternalAddons),
        _ => None,
    }
}

pub fn parse_agent_gui_settings_tab(tab: &str) -> Option<String> {
    let tab = match normalize_selector(tab).as_str() {
        "application" | "app" => "Application",
        "backup-manager" | "backup" | "backups" => "Backup Manager",
        "cleanup" => "Cleanup",
        "direct-download" | "download" => "Direct download",
        "scheduling" | "schedule" => "Scheduling",
        "customization" | "customisation" | "customize" | "customise" => "Customization",
        _ => return None,
    };
    Some(tab.to_string())
}

pub fn parse_agent_gui_game_space_settings_tab(
    tab: &str,
) -> Option<crate::ui::views::game_spaces::settings::GameSpaceSettingsTab> {
    use crate::ui::views::game_spaces::settings::GameSpaceSettingsTab;
    match normalize_selector(tab).as_str() {
        "game" | "game-space" | "general" => Some(GameSpaceSettingsTab::Game),
        "additional-search-folders" | "additional-folders" | "search-folders" | "folders" => {
            Some(GameSpaceSettingsTab::SearchFolders)
        }
        "ts3-plugin" | "ts3-plugins" | "ts3" | "teamspeak" => Some(GameSpaceSettingsTab::Ts3Plugin),
        _ => None,
    }
}

fn repo_state_name(state: RepoState) -> &'static str {
    match state {
        RepoState::Synced => "synced",
        RepoState::PendingUpdate => "pending-update",
        RepoState::Updating => "updating",
        RepoState::Unknown => "unknown",
    }
}

fn view_to_agent_name(view: FoxyView) -> &'static str {
    match view {
        FoxyView::RepositoryList => "repository-list",
        FoxyView::Settings => "settings",
        FoxyView::RepositorySettings => "repository-settings",
        FoxyView::RepositorySpaceSettings => "repository-space-settings",
        FoxyView::Help => "help",
        FoxyView::Changelog => "changelog",
        FoxyView::About => "about",
        FoxyView::AppUpdate => "app-update",
        FoxyView::VersionBrowser => "version-browser",
        FoxyView::SwiftyMigration => "swifty-migration",
        FoxyView::GameSpaces => "game-spaces",
        FoxyView::GameSpaceSettings => "game-space-settings",
        FoxyView::None => "none",
    }
}

pub fn parse_agent_gui_key(key: &str) -> Option<Key> {
    match normalize_selector(key).as_str() {
        "escape" | "esc" => Some(Key::Escape),
        "enter" | "return" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "arrowup" | "arrow-up" | "up" => Some(Key::ArrowUp),
        "arrowdown" | "arrow-down" | "down" => Some(Key::ArrowDown),
        "arrowleft" | "arrow-left" | "left" => Some(Key::ArrowLeft),
        "arrowright" | "arrow-right" | "right" => Some(Key::ArrowRight),
        "pageup" | "page-up" => Some(Key::PageUp),
        "pagedown" | "page-down" => Some(Key::PageDown),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "space" => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        // Fall back to egui's own parser for the long tail: letters (a-z),
        // digits, function keys (F1-F35), and punctuation. It is case-sensitive
        // for named keys, so hand it the original trimmed string.
        _ => Key::from_name(key.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_gui_view_aliases() {
        assert_eq!(
            parse_agent_gui_view("repository-list"),
            Some(FoxyView::RepositoryList)
        );
        assert_eq!(
            parse_agent_gui_view("repo-settings"),
            Some(FoxyView::RepositorySettings)
        );
        assert_eq!(
            parse_agent_gui_view("game-space-settings"),
            Some(FoxyView::GameSpaceSettings)
        );
        assert_eq!(parse_agent_gui_view("missing"), None);
    }

    #[test]
    fn parses_common_agent_gui_keys() {
        assert_eq!(parse_agent_gui_key("Escape"), Some(Key::Escape));
        assert_eq!(parse_agent_gui_key("arrow-down"), Some(Key::ArrowDown));
        assert_eq!(parse_agent_gui_key("unsupported"), None);
    }

    #[test]
    fn parses_letters_digits_and_function_keys_via_fallback() {
        assert_eq!(parse_agent_gui_key("a"), Some(Key::A));
        assert_eq!(parse_agent_gui_key("F"), Some(Key::F));
        assert_eq!(parse_agent_gui_key("F5"), Some(Key::F5));
        assert_eq!(parse_agent_gui_key("5"), Some(Key::Num5));
    }

    #[test]
    fn parses_pointer_buttons() {
        assert!(matches!(
            parse_pointer_button(None),
            Ok(PointerButton::Primary)
        ));
        assert!(matches!(
            parse_pointer_button(Some("Right")),
            Ok(PointerButton::Secondary)
        ));
        assert!(matches!(
            parse_pointer_button(Some("middle")),
            Ok(PointerButton::Middle)
        ));
        assert!(parse_pointer_button(Some("scroll")).is_err());
    }

    #[test]
    fn modifiers_map_ctrl_onto_command() {
        let mods = AgentGuiModifiers {
            ctrl: true,
            ..Default::default()
        }
        .to_egui();
        assert!(mods.ctrl);
        assert!(mods.command);
        assert!(!mods.shift);
        assert!(!mods.mac_cmd);
    }

    #[test]
    fn repo_state_names_are_stable_kebab_case() {
        assert_eq!(repo_state_name(RepoState::Synced), "synced");
        assert_eq!(repo_state_name(RepoState::PendingUpdate), "pending-update");
        assert_eq!(repo_state_name(RepoState::Updating), "updating");
        assert_eq!(repo_state_name(RepoState::Unknown), "unknown");
    }

    #[test]
    fn new_command_names_are_stable() {
        assert_eq!(
            AgentGuiCommand::Repositories {
                contains: None,
                limit: None,
            }
            .name(),
            "repositories"
        );
        assert_eq!(
            AgentGuiCommand::Addons {
                repository_index: None,
                tab: None,
                contains: None,
                enabled_only: false,
                limit: None,
            }
            .name(),
            "addons"
        );
        assert_eq!(AgentGuiCommand::Settings.name(), "settings");
        assert_eq!(AgentGuiCommand::Progress.name(), "progress");
        assert_eq!(AgentGuiCommand::Scale { percent: 100 }.name(), "scale");
        assert_eq!(
            AgentGuiCommand::Resize {
                width: 800.0,
                height: 600.0,
            }
            .name(),
            "resize"
        );
        assert_eq!(
            AgentGuiCommand::Profiles {
                repository_index: None,
                contains: None,
                limit: None,
            }
            .name(),
            "profiles"
        );
        assert_eq!(
            AgentGuiCommand::Missions {
                contains: None,
                limit: None,
            }
            .name(),
            "missions"
        );
        assert_eq!(
            AgentGuiCommand::Spaces {
                contains: None,
                limit: None,
            }
            .name(),
            "spaces"
        );
        assert_eq!(
            AgentGuiCommand::DownloadSummary {
                include_telemetry: false,
            }
            .name(),
            "download-summary"
        );
        assert_eq!(AgentGuiCommand::Toasts.name(), "toasts");
        assert_eq!(
            AgentGuiCommand::SetSetting {
                key: "debug-mode".to_string(),
                value: "true".to_string(),
            }
            .name(),
            "set-setting"
        );
    }

    #[test]
    fn parses_agent_gui_bool_spellings() {
        for truthy in ["true", "1", "on", "YES", " y "] {
            assert_eq!(parse_agent_gui_bool(truthy), Some(true), "{truthy:?}");
        }
        for falsy in ["false", "0", "off", "NO", " n "] {
            assert_eq!(parse_agent_gui_bool(falsy), Some(false), "{falsy:?}");
        }
        assert_eq!(parse_agent_gui_bool("maybe"), None);
    }

    #[test]
    fn toast_kind_names_are_stable() {
        assert_eq!(
            toast_kind_name(crate::ui::app::UiToastKind::Success),
            "success"
        );
        assert_eq!(toast_kind_name(crate::ui::app::UiToastKind::Error), "error");
    }

    #[test]
    fn new_wait_conditions_round_trip_through_json() {
        // The wire format tags the condition by `kind`; lock the kebab-case
        // names so the CLI client and driver stay in sync.
        for (condition, expected_kind) in [
            (
                AgentGuiWaitCondition::Toast {
                    text: "Saved".to_string(),
                },
                "toast",
            ),
            (
                AgentGuiWaitCondition::BusyReasonCleared {
                    reason: "core-sync".to_string(),
                },
                "busy-reason-cleared",
            ),
            (AgentGuiWaitCondition::DownloadComplete, "download-complete"),
            (AgentGuiWaitCondition::FpsAbove { fps: 30.0 }, "fps-above"),
            (
                AgentGuiWaitCondition::NodeVisible {
                    id: "footer.help".to_string(),
                },
                "node-visible",
            ),
        ] {
            let value = serde_json::to_value(&condition).unwrap();
            assert_eq!(
                value.get("kind").and_then(Value::as_str),
                Some(expected_kind)
            );
        }
    }

    #[test]
    fn parses_repository_settings_tab_aliases() {
        assert_eq!(
            parse_agent_gui_repository_settings_tab("external-addons"),
            Some(RepositorySettingsTab::ExternalAddons)
        );
        assert_eq!(
            parse_agent_gui_repository_settings_tab("Addons"),
            Some(RepositorySettingsTab::Addons)
        );
        assert_eq!(parse_agent_gui_repository_settings_tab("missing"), None);
    }

    #[test]
    fn parses_settings_tab_aliases() {
        assert_eq!(
            parse_agent_gui_settings_tab("application").as_deref(),
            Some("Application")
        );
        assert_eq!(
            parse_agent_gui_settings_tab("backup-manager").as_deref(),
            Some("Backup Manager")
        );
        assert_eq!(
            parse_agent_gui_settings_tab("direct-download").as_deref(),
            Some("Direct download")
        );
        assert_eq!(
            parse_agent_gui_settings_tab("scheduling").as_deref(),
            Some("Scheduling")
        );
        assert_eq!(
            parse_agent_gui_settings_tab("customization").as_deref(),
            Some("Customization")
        );
        assert_eq!(
            parse_agent_gui_settings_tab("additional-search-folders"),
            None
        );
        assert_eq!(parse_agent_gui_settings_tab("ts3"), None);
        assert_eq!(parse_agent_gui_settings_tab("missing"), None);
    }

    #[test]
    fn parses_game_space_settings_tab_aliases() {
        use crate::ui::views::game_spaces::settings::GameSpaceSettingsTab;
        assert_eq!(
            parse_agent_gui_game_space_settings_tab("game"),
            Some(GameSpaceSettingsTab::Game)
        );
        assert_eq!(
            parse_agent_gui_game_space_settings_tab("additional-search-folders"),
            Some(GameSpaceSettingsTab::SearchFolders)
        );
        assert_eq!(
            parse_agent_gui_game_space_settings_tab("ts3"),
            Some(GameSpaceSettingsTab::Ts3Plugin)
        );
        assert_eq!(parse_agent_gui_game_space_settings_tab("missing"), None);
    }

    #[test]
    fn command_names_are_stable() {
        for (command, expected) in [
            (AgentGuiCommand::Health, "health"),
            (
                AgentGuiCommand::Focus {
                    target: None,
                    clear: true,
                },
                "focus",
            ),
            (
                AgentGuiCommand::Nav {
                    count: 1,
                    reverse: false,
                },
                "nav",
            ),
            (
                AgentGuiCommand::Fill {
                    target: "addons-filter".to_string(),
                    value: "ace".to_string(),
                },
                "fill",
            ),
            (AgentGuiCommand::Filters, "filters"),
            (
                AgentGuiCommand::SetFilter {
                    name: "favorites-only".to_string(),
                    value: "true".to_string(),
                },
                "set-filter",
            ),
            (
                AgentGuiCommand::Select {
                    repository: Some(0),
                    server: None,
                    mission: None,
                    space: None,
                },
                "select",
            ),
            (
                AgentGuiCommand::Window {
                    action: "minimize".to_string(),
                },
                "window",
            ),
            (AgentGuiCommand::Settle { frames: 2 }, "settle"),
            (AgentGuiCommand::StableRender { on: true }, "stable-render"),
            (
                AgentGuiCommand::Assert {
                    field: "view".to_string(),
                    equals: Some("settings".to_string()),
                    contains: None,
                    repository_index: None,
                },
                "assert",
            ),
            (
                AgentGuiCommand::ContextMenu {
                    id: None,
                    x: Some(1.0),
                    y: Some(2.0),
                },
                "context-menu",
            ),
            (
                AgentGuiCommand::MenuSelect {
                    item: "Remove".to_string(),
                },
                "menu-select",
            ),
            (
                AgentGuiCommand::Inventory {
                    contains: None,
                    folder: None,
                    source: None,
                    limit: None,
                },
                "inventory",
            ),
            (
                AgentGuiCommand::PendingUpdates {
                    repository_index: None,
                    contains: None,
                    limit: None,
                    include_files: false,
                },
                "pending-updates",
            ),
            (AgentGuiCommand::AppUpdate, "app-update"),
            (
                AgentGuiCommand::Memory {
                    history: false,
                    textures: false,
                },
                "memory",
            ),
            (AgentGuiCommand::ArmaProfiles, "arma-profiles"),
            (
                AgentGuiCommand::Backups {
                    contains: None,
                    limit: None,
                },
                "backups",
            ),
        ] {
            assert_eq!(command.name(), expected);
        }
    }

    #[test]
    fn snapshot_command_round_trips_projection_fields() {
        let command = AgentGuiCommand::Snapshot {
            fields: Some(vec!["view".to_string(), "fps".to_string()]),
            since_frame: Some(42),
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(
            value.get("command").and_then(Value::as_str),
            Some("snapshot")
        );
        assert_eq!(value.get("since_frame").and_then(Value::as_u64), Some(42));
        // An older snapshot request (no extra fields) still deserializes.
        let legacy: AgentGuiCommand = serde_json::from_str(r#"{"command":"snapshot"}"#).unwrap();
        assert!(matches!(
            legacy,
            AgentGuiCommand::Snapshot {
                fields: None,
                since_frame: None
            }
        ));
    }

    #[test]
    fn project_object_fields_keeps_only_requested_keys() {
        let value = json!({ "view": "settings", "fps": 60.0, "busy": false });
        let projected = project_object_fields(value, &["view".to_string(), "busy".to_string()]);
        assert_eq!(projected, json!({ "view": "settings", "busy": false }));
    }

    #[test]
    fn json_pointer_lookup_walks_dotted_paths() {
        let value = json!({ "nodes": [{ "rect": { "x": 12.0 } }] });
        assert_eq!(
            json_pointer_lookup(&value, "nodes.0.rect.x"),
            Some(json!(12.0))
        );
        assert_eq!(json_pointer_lookup(&value, "nodes.5"), None);
        assert_eq!(json_pointer_lookup(&value, ""), Some(value));
    }

    #[test]
    fn json_value_to_plain_string_unquotes_strings() {
        assert_eq!(json_value_to_plain_string(&json!("settings")), "settings");
        assert_eq!(json_value_to_plain_string(&json!(60)), "60");
        assert_eq!(json_value_to_plain_string(&json!(true)), "true");
        assert_eq!(json_value_to_plain_string(&Value::Null), "null");
    }

    #[test]
    fn path_basename_redacts_to_final_component() {
        assert_eq!(path_basename("C:\\Users\\x\\addons\\@ace"), "@ace");
        assert_eq!(path_basename("/home/u/addons/@cba"), "@cba");
        assert_eq!(path_basename("@plain"), "@plain");
    }

    #[test]
    fn focus_target_registry_resolves_known_target() {
        assert_eq!(
            agent_gui_focus_target_id("add-repository-input"),
            Some(egui::Id::new("agent.add-repository-input"))
        );
        assert_eq!(agent_gui_focus_target_id("nope"), None);
        assert!(agent_gui_focus_target_names().contains(&"add-repository-input"));
    }

    #[test]
    fn annotation_sidecar_path_appends_suffix() {
        let path = annotation_sidecar_path(Path::new("shots/a.png"));
        assert!(path.ends_with("a.png.nodes.json"));
    }

    fn test_node(id: &str, role: &str, text: &str) -> AgentGuiNode {
        AgentGuiNode {
            id: id.to_string(),
            role: role.to_string(),
            text: text.to_string(),
            enabled: true,
            focused: false,
            rect: Some(AgentGuiRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            }),
        }
    }

    fn test_snapshot(view: &str, frame: u64, nodes: Vec<AgentGuiNode>) -> AgentGuiSnapshot {
        let texts = nodes.iter().map(|node| node.text.clone()).collect();
        AgentGuiSnapshot {
            view: view.to_string(),
            update_modal_open: false,
            fps: 60.0,
            startup_frame_rendered: true,
            busy: false,
            active_modal_count: 0,
            active_modals: Vec::new(),
            pointer: None,
            focused: None,
            repositories_count: 0,
            selected_repository: None,
            settings_tab: None,
            repository_settings_tab: None,
            frame,
            cumulative_pass_nr: frame,
            pixels_per_point: 1.0,
            zoom_factor: 1.0,
            busy_reasons: Vec::new(),
            content_rect: AgentGuiRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            texts,
            nodes,
        }
    }

    #[test]
    fn new_command_names_are_stable_for_plan_additions() {
        for (command, expected) in [
            (
                AgentGuiCommand::Invoke {
                    action: Some("start-sync".to_string()),
                    params: Value::Null,
                    allow_destructive: false,
                    list_actions: false,
                },
                "invoke",
            ),
            (
                AgentGuiCommand::Batch {
                    steps: Vec::new(),
                    stop_on_error: true,
                },
                "batch",
            ),
            (
                AgentGuiCommand::Diff {
                    baseline: "last".to_string(),
                },
                "diff",
            ),
            (
                AgentGuiCommand::Query {
                    expr: "snapshot.view".to_string(),
                },
                "query",
            ),
            (
                AgentGuiCommand::Restore {
                    name: "base".to_string(),
                },
                "restore",
            ),
            (
                AgentGuiCommand::Clock {
                    action: "advance".to_string(),
                    ms: Some(1000),
                },
                "clock",
            ),
            (
                AgentGuiCommand::Dialog {
                    action: "pending".to_string(),
                    path: None,
                    cancel: false,
                },
                "dialog",
            ),
        ] {
            assert_eq!(command.name(), expected);
        }
    }

    #[test]
    fn list_actions_flags_destructive_intents() {
        let value = agent_gui_list_actions_value();
        let actions = value.get("actions").and_then(Value::as_array).unwrap();
        let find = |name: &str| {
            actions
                .iter()
                .find(|action| action.get("name").and_then(Value::as_str) == Some(name))
                .unwrap()
        };
        assert_eq!(
            find("open-settings")
                .get("destructive")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            find("start-sync")
                .get("destructive")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            find("launch-game")
                .get("destructive")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn node_action_maps_known_ids() {
        assert_eq!(
            agent_gui_node_action("header.settings"),
            json!("open-settings")
        );
        assert_eq!(agent_gui_node_action("footer.help"), json!("open-help"));
        assert_eq!(agent_gui_node_action("view.current"), Value::Null);
    }

    #[test]
    fn snapshot_diff_reports_node_text_and_field_changes() {
        let base = test_snapshot(
            "repository-list",
            10,
            vec![test_node("a", "button", "Alpha")],
        );
        let current = test_snapshot("settings", 12, vec![test_node("b", "button", "Beta")]);
        let diff = agent_gui_snapshot_diff(&base, &current);
        assert_eq!(
            diff["added_nodes"][0]["id"].as_str(),
            Some("b"),
            "new node should be added"
        );
        assert_eq!(
            diff["removed_nodes"][0]["id"].as_str(),
            Some("a"),
            "old node should be removed"
        );
        assert_eq!(
            diff["changed_fields"]["view"]["to"].as_str(),
            Some("settings"),
            "view change should be captured"
        );
        // `fps` is on the noise denylist and must not appear even if it differs.
        assert!(diff["changed_fields"].get("fps").is_none());
        let text_added: Vec<&str> = diff["text_added"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(text_added.contains(&"Beta"));
    }

    #[test]
    fn resolve_baseline_supports_last_and_frame() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut runtime = AgentGuiRuntime::new(
            rx,
            AgentGuiSession {
                pid: 1,
                host: "127.0.0.1".to_string(),
                port: 0,
                token: "t".to_string(),
                session_file: PathBuf::from("session.json"),
            },
        );
        runtime.push_diff_baseline(test_snapshot("repository-list", 100, vec![]));
        runtime.push_diff_baseline(test_snapshot("settings", 200, vec![]));
        assert_eq!(
            agent_gui_resolve_baseline(&runtime, "last").map(|s| s.frame),
            Some(200)
        );
        assert_eq!(
            agent_gui_resolve_baseline(&runtime, "frame:100").map(|s| s.frame),
            Some(100)
        );
        // No exact match -> most recent at or before the target.
        assert_eq!(
            agent_gui_resolve_baseline(&runtime, "frame:150").map(|s| s.frame),
            Some(100)
        );
        assert!(agent_gui_resolve_baseline(&runtime, "bogus").is_none());
    }

    #[test]
    fn events_filter_by_kind_and_since() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut runtime = AgentGuiRuntime::new(
            rx,
            AgentGuiSession {
                pid: 1,
                host: "127.0.0.1".to_string(),
                port: 0,
                token: "t".to_string(),
                session_file: PathBuf::from("session.json"),
            },
        );
        runtime.record_event("click", json!({}));
        runtime.record_event("view-change", json!({}));
        runtime.record_event("click", json!({}));

        let all = agent_gui_events_value(&runtime, None, None, None);
        assert_eq!(all["count"].as_u64(), Some(3));

        let clicks = agent_gui_events_value(&runtime, Some(&["click".to_string()]), None, None);
        assert_eq!(clicks["count"].as_u64(), Some(2));

        let since = agent_gui_events_value(&runtime, None, Some(2), None);
        assert_eq!(
            since["count"].as_u64(),
            Some(1),
            "only the third event is newer than gen 2"
        );
    }
}
