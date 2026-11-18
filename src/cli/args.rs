use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod reforger_admin;
pub use reforger_admin::*;

#[derive(Parser, Debug)]
#[command(name = "foxy")]
#[command(about = "Foxy command line interface")]
#[command(version = crate::build_info::clap_version())]
pub struct CliArgs {
    #[arg(
        long,
        global = true,
        help = "Override config root directory (contains app_settings.json, games.json, and per game space data under games/<space>/)"
    )]
    pub config_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "Emit machine-readable JSON output")]
    pub json: bool,
    #[arg(
        long,
        global = true,
        help = "Emit machine-readable TOON output; implies --json behavior"
    )]
    pub toon: bool,
    #[arg(long, global = true, help = "Reduce progress and informational output")]
    pub quiet: bool,
    #[arg(
        long,
        global = true,
        help = "Disable live progress updates (screen-reader friendly alternative to --quiet)"
    )]
    pub no_progress: bool,
    #[arg(
        long,
        global = true,
        help = "Confirm destructive operations (required where applicable)"
    )]
    pub yes: bool,
    #[arg(
        long,
        global = true,
        help = "Preview command result without writing changes (supported commands only)"
    )]
    pub dry_run: bool,
    #[arg(
        long,
        global = true,
        help = "Wipe and rebuild the entire local database (schema-upgrade recovery); requires --yes"
    )]
    pub wipe_db: bool,
    #[arg(
        long,
        global = true,
        help = "agent-gui: print just the inner payload (drop the AgentGuiResponse envelope)"
    )]
    pub flat: bool,
    #[arg(
        long,
        global = true,
        help = "agent-gui: print only the value at this dotted path inside the payload (e.g. view, nodes.0.rect)"
    )]
    pub field: Option<String>,
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Launch the desktop UI from terminal.
    Ui(UiArgs),
    /// Drive a running Foxy UI session started with `ui --agent-gui`.
    AgentGui {
        #[command(subcommand)]
        command: AgentGuiCommand,
    },
    /// Print the current Foxy version.
    Version,
    #[command(hide = true)]
    ContentAcquisitionWorker,
    /// Inspect or modify global settings.
    Settings {
        #[command(subcommand)]
        command: Box<SettingsCommand>,
    },
    /// Manage repositories and repository-level operations.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Shortcut for `repo sync`.
    Sync(RepoSyncArgs),
    /// List or modify addon enablement state.
    Addon {
        #[command(subcommand)]
        command: AddonCommand,
    },
    /// Manage repository launch profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// List or sync repository spaces.
    Space {
        #[command(subcommand)]
        command: SpaceCommand,
    },
    /// Manage game spaces (list, switch, create, remove).
    Game {
        #[command(subcommand)]
        command: GameCommand,
    },
    /// Export or import portable Foxy config packs.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage Steam Workshop items for the active game space.
    Workshop {
        #[command(subcommand)]
        command: WorkshopCommand,
    },
    #[command(hide = true)]
    SteamHelper {
        #[command(subcommand)]
        command: SteamHelperCommand,
    },
    /// Inspect Arma 3 server metadata.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Download repository/addon/file content directly from URL.
    DirectDownload(DirectDownloadArgs),
    /// Build or execute Arma 3 launch command from repository/profile.
    Launch(LaunchArgs),
}

#[derive(Args, Debug, Default)]
pub struct UiArgs {
    #[arg(
        long,
        value_enum,
        help = "Select client or server mode for this UI session without saving it"
    )]
    pub mode: Option<AppModeArg>,
    #[arg(
        long,
        help = "Enable debug mode for this UI session (not persisted to settings)"
    )]
    pub debug_mode: bool,
    #[arg(
        long,
        visible_alias = "agents",
        help = "Start a local agent GUI driver for desktop UI automation"
    )]
    pub agent_gui: bool,
    #[arg(
        long,
        default_value_t = 0,
        requires = "agent_gui",
        help = "Loopback TCP port for --agent-gui (0 lets the OS choose)"
    )]
    pub agent_port: u16,
    #[arg(
        long = "debug-modal",
        value_enum,
        value_name = "MODAL",
        help = "Force a startup modal open with placeholder data for inspection (repeatable). Its real actions stay disabled."
    )]
    pub debug_modals: Vec<crate::ui::app::debug_modals::DebugModal>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AppModeArg {
    Client,
    Server,
}

impl From<AppModeArg> for crate::core::app_mode::AppMode {
    fn from(value: AppModeArg) -> Self {
        match value {
            AppModeArg::Client => Self::Client,
            AppModeArg::Server => Self::Server,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum AgentGuiCommand {
    /// Show running driver status.
    Status,
    /// Open a Foxy view by stable name.
    OpenView(AgentGuiOpenViewArgs),
    /// Return structured UI state (optionally projected / delta-gated).
    Snapshot(AgentGuiSnapshotArgs),
    /// Return visible/known UI text.
    Text(AgentGuiTextArgs),
    /// Find known semantic UI nodes.
    Find(AgentGuiFindArgs),
    /// Click a semantic target or logical coordinate.
    Click(AgentGuiClickArgs),
    /// Send a scroll event.
    Scroll(AgentGuiScrollArgs),
    /// Move the pointer to a node or coordinate (hover / tooltip trigger).
    Hover(AgentGuiHoverArgs),
    /// Press a mouse button down without releasing it (start of a drag).
    MouseDown(AgentGuiMouseArgs),
    /// Release a held mouse button (end of a drag).
    MouseUp(AgentGuiMouseArgs),
    /// Send a keyboard key press/release.
    Key(AgentGuiKeyArgs),
    /// Type text into the focused widget.
    Type(AgentGuiTypeArgs),
    /// Capture an egui screenshot to a PNG file.
    Screenshot(AgentGuiScreenshotArgs),
    /// Read the UI FPS probe.
    Fps,
    /// Wait for a view, text, modal, or idle state.
    Wait(AgentGuiWaitArgs),
    /// Read recent in-memory activity-log entries (incl. egui warnings).
    Logs(AgentGuiLogsArgs),
    /// List configured repositories with sync state and pending updates.
    Repositories(AgentGuiRepositoriesArgs),
    /// List structured addon rows for a repository tab (rows are not nodes).
    Addons(AgentGuiAddonsArgs),
    /// Dump the live, effective settings the running app currently holds.
    Settings,
    /// Read live download/sync/recheck progress and busy reasons.
    Progress,
    /// Set the global UI scale percent (reproduces high-DPI relayout paths).
    Scale(AgentGuiScaleArgs),
    /// Resize the window to a logical inner size (reproduces large-window paths).
    Resize(AgentGuiResizeArgs),
    /// List launch profiles for a repository (flags + addon override counts).
    Profiles(AgentGuiProfilesArgs),
    /// List cached editor missions for the viewed repository.
    Missions(AgentGuiMissionsArgs),
    /// List repository spaces with attached-repo counts and bulk progress.
    Spaces(AgentGuiSpacesArgs),
    /// Read the last completed download summary (and optional telemetry).
    DownloadSummary(AgentGuiDownloadSummaryArgs),
    /// Read the current user-feedback toast, if one is showing.
    Toasts,
    /// Mutate a single live setting on the running app and observe it react.
    SetSetting(AgentGuiSetSettingArgs),
    /// Build/version + renderer preflight (the first call of a session).
    Health,
    /// Set keyboard focus on a named text field, or clear focus.
    Focus(AgentGuiFocusArgs),
    /// Send Tab / Shift+Tab presses and report the resulting focused widget.
    Nav(AgentGuiNavArgs),
    /// Focus + clear + set a named text field in one step.
    Fill(AgentGuiFillArgs),
    /// Read the current list filter values.
    Filters,
    /// Write one list filter (string or boolean) and request a repaint.
    SetFilter(AgentGuiSetFilterArgs),
    /// Non-destructively select a repository / server / mission / space.
    Select(AgentGuiSelectArgs),
    /// Window/tray lifecycle: minimize/restore/maximize/focus/hide-to-tray/show.
    Window(AgentGuiWindowArgs),
    /// Park until N frames render, then return the post-input snapshot.
    Settle(AgentGuiSettleArgs),
    /// Hold for a real-time span while the app keeps running, then return the snapshot.
    Pause(AgentGuiPauseArgs),
    /// Show a scene caption over the app for a real-time span, then return the snapshot.
    Caption(AgentGuiCaptionArgs),
    /// Toggle stable-render mode for byte-stable screenshots.
    StableRender(AgentGuiStableRenderArgs),
    /// Assert one observed field equals/contains an expected value.
    Assert(AgentGuiAssertArgs),
    /// Secondary-click a target to open its egui context menu.
    ContextMenu(AgentGuiContextMenuArgs),
    /// Activate an open menu/popup entry by its visible label.
    MenuSelect(AgentGuiMenuSelectArgs),
    /// Global cross-folder addon inventory (shared-addon investigations).
    Inventory(AgentGuiInventoryArgs),
    /// The planned update set before a sync (diff against download-summary).
    PendingUpdates(AgentGuiPendingUpdatesArgs),
    /// The self-update flow status plus the configured mode/url.
    AppUpdate,
    /// The latest memory-diagnostics sample + texture-tracking totals.
    Memory(AgentGuiMemoryArgs),
    /// OS-level Arma 3 player profiles that drive the -profiles launch arg.
    ArmaProfiles,
    /// Addon-backup records with per-addon counts and retention state.
    Backups(AgentGuiBackupsArgs),
    /// Drive a named app-action by intent (command palette); --list-actions.
    Invoke(AgentGuiInvokeArgs),
    /// Run a JSON array of commands server-side in one round-trip (--stdin).
    Batch(AgentGuiBatchArgs),
    /// Field-level delta vs a stored baseline (last, or frame:<n>).
    Diff(AgentGuiDiffArgs),
    /// First-class drag gesture from one node/coordinate to another.
    Drag(AgentGuiDragArgs),
    /// One JMESPath query over the union of structured app state.
    Query(AgentGuiQueryArgs),
    /// Save the serializable UI state under a name (--list to enumerate).
    Checkpoint(AgentGuiCheckpointArgs),
    /// Roll the UI back to a named checkpoint (UI state only).
    Restore(AgentGuiRestoreArgs),
    /// Deep single-node introspection by id, or hit-test at a coordinate.
    Element(AgentGuiElementArgs),
    /// Recent semantic UI events (the causal counterpart to logs).
    Events(AgentGuiEventsArgs),
    /// Virtual time control: advance/freeze/resume/status the UI clock.
    Clock(AgentGuiClockArgs),
    /// Native file/folder picker automation (expect/pending/clear).
    Dialog(AgentGuiDialogArgs),
    /// Stream newline-delimited command JSON from stdin over one connection.
    Exec(AgentGuiExecArgs),
    /// Run a committed scenario file (a sequence of agent-gui steps).
    Scenario(AgentGuiScenarioArgs),
    /// Seed the isolated config dir with a known fixture (JSON config only).
    Fixture(AgentGuiFixtureArgs),
    /// Request the running UI session to close.
    Close,
}

/// Keyboard/pointer modifiers, flattened into the input subcommands.
#[derive(Args, Debug, Default, Clone, Copy)]
pub struct AgentGuiModifierArgs {
    #[arg(long, help = "Hold Ctrl while sending the event")]
    pub ctrl: bool,
    #[arg(long, help = "Hold Shift while sending the event")]
    pub shift: bool,
    #[arg(long, help = "Hold Alt while sending the event")]
    pub alt: bool,
    #[arg(long, help = "Hold Command/Super while sending the event")]
    pub command: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiOpenViewArgs {
    #[arg(help = "View name such as repository-list, settings, help, or about")]
    pub view: String,
    #[arg(long, help = "Zero-based repository index for repository-settings")]
    pub repo_index: Option<usize>,
    #[arg(
        long,
        help = "Tab for settings or repository-settings, e.g. customization or external-addons"
    )]
    pub tab: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentGuiTextArgs {
    #[arg(long, help = "Only return texts containing this value")]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of texts to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiFindArgs {
    #[arg(long, help = "Match node text")]
    pub text: Option<String>,
    #[arg(long, help = "Match node role")]
    pub role: Option<String>,
    #[arg(long, help = "Match node ID")]
    pub id: Option<String>,
    #[arg(long, help = "Only include visible nodes")]
    pub visible_only: bool,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("click_target")
        .required(true)
        .args(&["text", "id", "x"])
))]
pub struct AgentGuiClickArgs {
    #[arg(long, help = "Click by semantic text")]
    pub text: Option<String>,
    #[arg(long, help = "Click by semantic node ID")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Logical x coordinate"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Logical y coordinate"
    )]
    pub y: Option<f32>,
    #[arg(long, help = "Mouse button: primary (default), secondary, or middle")]
    pub button: Option<String>,
    #[arg(long, help = "Send a double-click instead of a single click")]
    pub double: bool,
    #[command(flatten)]
    pub modifiers: AgentGuiModifierArgs,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("hover_target")
        .required(true)
        .args(&["id", "x"])
))]
pub struct AgentGuiHoverArgs {
    #[arg(long, help = "Move to a semantic node ID")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Logical x coordinate"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Logical y coordinate"
    )]
    pub y: Option<f32>,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("mouse_target")
        .required(true)
        .args(&["id", "x"])
))]
pub struct AgentGuiMouseArgs {
    #[arg(long, help = "Target a semantic node ID")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Logical x coordinate"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Logical y coordinate"
    )]
    pub y: Option<f32>,
    #[arg(long, help = "Mouse button: primary (default), secondary, or middle")]
    pub button: Option<String>,
    #[command(flatten)]
    pub modifiers: AgentGuiModifierArgs,
}

#[derive(Args, Debug)]
pub struct AgentGuiScrollArgs {
    #[arg(long, help = "Optional semantic node ID to target")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Logical x coordinate"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Logical y coordinate"
    )]
    pub y: Option<f32>,
    #[arg(
        long,
        default_value_t = 0.0,
        allow_hyphen_values = true,
        help = "Horizontal scroll delta in egui points (negative scrolls left)"
    )]
    pub dx: f32,
    #[arg(
        long,
        default_value_t = 0.0,
        allow_hyphen_values = true,
        help = "Vertical scroll delta in egui points (negative scrolls the content down/reveals lower rows)"
    )]
    pub dy: f32,
    #[command(flatten)]
    pub modifiers: AgentGuiModifierArgs,
}

#[derive(Args, Debug)]
pub struct AgentGuiKeyArgs {
    #[arg(
        long,
        help = "Key name such as Escape, Enter, Tab, ArrowDown, a, F5, 5"
    )]
    pub key: String,
    #[command(flatten)]
    pub modifiers: AgentGuiModifierArgs,
}

#[derive(Args, Debug)]
pub struct AgentGuiTypeArgs {
    #[arg(help = "Text to type into the focused widget")]
    pub text: String,
}

#[derive(Args, Debug)]
pub struct AgentGuiScreenshotArgs {
    #[arg(long, help = "PNG output path")]
    pub output: PathBuf,
    #[arg(
        long,
        help = "Overlay node rects + ids and the pointer, and write a <output>.nodes.json sidecar"
    )]
    pub annotate: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiSnapshotArgs {
    #[arg(
        long,
        value_delimiter = ',',
        help = "Project only these top-level snapshot keys (comma-separated), e.g. view,fps,busy"
    )]
    pub fields: Option<Vec<String>>,
    #[arg(
        long,
        help = "Return {changed:false} when no frame has rendered since this cumulative frame number"
    )]
    pub since_frame: Option<u64>,
}

#[derive(Args, Debug)]
pub struct AgentGuiFocusArgs {
    #[arg(help = "Focus target name (e.g. add-repository-input)")]
    pub target: Option<String>,
    #[arg(long, help = "Clear keyboard focus instead of setting it")]
    pub clear: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiNavArgs {
    #[arg(long, default_value_t = 1, help = "Number of Tab presses to send")]
    pub count: u32,
    #[arg(long, help = "Traverse backwards (Shift+Tab)")]
    pub reverse: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiFillArgs {
    #[arg(help = "Text field target name (e.g. add-repository-input, addons-filter)")]
    pub target: String,
    #[arg(help = "Value to set")]
    pub value: String,
}

#[derive(Args, Debug)]
pub struct AgentGuiSetFilterArgs {
    #[arg(help = "Filter name (e.g. addons-filter, favorites-only)")]
    pub name: String,
    #[arg(help = "New value (string, or bool for toggle filters)")]
    pub value: String,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("select_target")
        .required(true)
        .multiple(true)
        .args(&["repository", "server", "mission", "space"])
))]
pub struct AgentGuiSelectArgs {
    #[arg(long, help = "Select repository by zero-based index")]
    pub repository: Option<usize>,
    #[arg(long, help = "Select server by zero-based index in the selected repo")]
    pub server: Option<usize>,
    #[arg(long, help = "Select mission by zero-based index in the cached list")]
    pub mission: Option<usize>,
    #[arg(long, help = "Select repository space by id")]
    pub space: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentGuiWindowArgs {
    #[arg(help = "minimize | restore | maximize | unmaximize | focus | hide-to-tray | show")]
    pub action: String,
}

#[derive(Args, Debug)]
pub struct AgentGuiSettleArgs {
    #[arg(
        long,
        default_value_t = 2,
        help = "Frames to wait for before returning the snapshot"
    )]
    pub frames: u64,
}

#[derive(Args, Debug)]
pub struct AgentGuiPauseArgs {
    #[arg(long, help = "Milliseconds to hold before returning the snapshot")]
    pub ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentGuiCaptionArgs {
    #[arg(help = "Scene title shown in the caption (English only, never translated)")]
    pub title: String,
    #[arg(long, help = "Optional second line describing what the scene shows")]
    pub detail: Option<String>,
    #[arg(
        long,
        default_value_t = 3000,
        help = "Milliseconds to show the caption before returning the snapshot"
    )]
    pub ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentGuiStableRenderArgs {
    #[arg(
        // A positional bool defaults to a flag action; force it to take a value
        // so `stable-render true|false` parses.
        action = clap::ArgAction::Set,
        value_parser = clap::value_parser!(bool),
        help = "true to enable stable-render mode, false to disable"
    )]
    pub on: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiAssertArgs {
    #[arg(
        // Distinct id so this positional does not collide with the global
        // `--field` projection arg (same id would bind the positional value to
        // `--field` and null out the assert payload).
        id = "assert_field",
        help = "Field path: snapshot key, or settings.<path> / progress.<path>"
    )]
    pub field: String,
    #[arg(long, help = "Assert the observed value equals this")]
    pub equals: Option<String>,
    #[arg(long, help = "Assert the observed value contains this substring")]
    pub contains: Option<String>,
    #[arg(long, help = "Repository index for repository-scoped assertions")]
    pub repository_index: Option<usize>,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("context_menu_target")
        .required(true)
        .args(&["id", "x"])
))]
pub struct AgentGuiContextMenuArgs {
    #[arg(long, help = "Target a semantic node id")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Logical x coordinate"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Logical y coordinate"
    )]
    pub y: Option<f32>,
}

#[derive(Args, Debug)]
pub struct AgentGuiMenuSelectArgs {
    #[arg(help = "Visible label text of the menu/popup entry to activate")]
    pub item: String,
}

#[derive(Args, Debug)]
pub struct AgentGuiInventoryArgs {
    #[arg(long, help = "Only return addons whose name contains this value")]
    pub contains: Option<String>,
    #[arg(
        long,
        help = "Only return addons whose folder basename contains this value"
    )]
    pub folder: Option<String>,
    #[arg(
        long,
        help = "Only return addons whose source/origin contains this value"
    )]
    pub source: Option<String>,
    #[arg(long, help = "Maximum number of rows to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiPendingUpdatesArgs {
    #[arg(long, help = "Restrict to this zero-based repository index")]
    pub repo_index: Option<usize>,
    #[arg(long, help = "Only return mods whose name contains this value")]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of mods per repository to return")]
    pub limit: Option<usize>,
    #[arg(long, help = "Include per-file diffs for each mod")]
    pub include_files: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiMemoryArgs {
    #[arg(long, help = "Include the full per-sample history series")]
    pub history: bool,
    #[arg(long, help = "Include per-texture tracking rows (icons / repo images)")]
    pub textures: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiBackupsArgs {
    #[arg(
        long,
        help = "Only return backups whose addon name contains this value"
    )]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of rows to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiExecArgs {
    #[arg(
        long,
        help = "Read newline-delimited command JSON from stdin over one persistent connection"
    )]
    pub stdin: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiScenarioArgs {
    #[arg(help = "Path to a scenario JSON file (array of step objects)")]
    pub file: PathBuf,
}

#[derive(Args, Debug)]
pub struct AgentGuiFixtureArgs {
    #[arg(help = "Fixture JSON file describing the config files to seed")]
    pub file: PathBuf,
}

#[derive(Args, Debug)]
pub struct AgentGuiInvokeArgs {
    #[arg(help = "Action name (e.g. start-sync); omit with --list-actions")]
    pub action: Option<String>,
    #[arg(long, help = "Repository index parameter for repo-scoped actions")]
    pub repo_index: Option<usize>,
    #[arg(long, help = "Profile name parameter for apply-profile")]
    pub profile: Option<String>,
    #[arg(
        long,
        help = "Extra params as a JSON object merged with the flags above"
    )]
    pub params: Option<String>,
    #[arg(long, help = "Permit a core/disk-mutating action to run")]
    pub allow_destructive: bool,
    #[arg(long, help = "Enumerate the action registry instead of running one")]
    pub list_actions: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiBatchArgs {
    #[arg(
        long,
        help = "Read the command array (JSON) from stdin instead of --steps"
    )]
    pub stdin: bool,
    #[arg(long, help = "Inline JSON array of command objects")]
    pub steps: Option<String>,
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        help = "Stop the pipeline at the first failing step"
    )]
    pub stop_on_error: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiDiffArgs {
    #[arg(
        long,
        default_value = "last",
        help = "Baseline to diff against: last, or frame:<n>"
    )]
    pub baseline: String,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("drag_from")
        .required(true)
        .args(&["from_id", "from_x"])
))]
#[command(group(
    ArgGroup::new("drag_to")
        .required(true)
        .args(&["to_id", "to_x"])
))]
pub struct AgentGuiDragArgs {
    #[arg(long, help = "Drag from this semantic node id")]
    pub from_id: Option<String>,
    #[arg(
        long,
        requires = "from_y",
        allow_hyphen_values = true,
        help = "Drag start x (logical points)"
    )]
    pub from_x: Option<f32>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Drag start y (logical points)"
    )]
    pub from_y: Option<f32>,
    #[arg(long, help = "Drag to this semantic node id")]
    pub to_id: Option<String>,
    #[arg(
        long,
        requires = "to_y",
        allow_hyphen_values = true,
        help = "Drag end x (logical points)"
    )]
    pub to_x: Option<f32>,
    #[arg(long, allow_hyphen_values = true, help = "Drag end y (logical points)")]
    pub to_y: Option<f32>,
    #[arg(
        long,
        default_value_t = 6,
        help = "Interpolated move events between down and up"
    )]
    pub steps: u32,
    #[arg(long, help = "Mouse button: primary (default), secondary, or middle")]
    pub button: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentGuiQueryArgs {
    #[arg(
        id = "query_expr",
        help = "JMESPath expression over the union app-state document"
    )]
    pub expr: String,
}

#[derive(Args, Debug)]
pub struct AgentGuiCheckpointArgs {
    #[arg(help = "Checkpoint name; omit with --list")]
    pub name: Option<String>,
    #[arg(long, help = "List saved checkpoint names instead of saving")]
    pub list: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiRestoreArgs {
    #[arg(help = "Checkpoint name to restore")]
    pub name: String,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("element_target")
        .required(true)
        .args(&["id", "x"])
))]
pub struct AgentGuiElementArgs {
    #[arg(long, help = "Inspect the node with this id")]
    pub id: Option<String>,
    #[arg(
        long,
        requires = "y",
        allow_hyphen_values = true,
        help = "Hit-test x (logical points)"
    )]
    pub x: Option<f32>,
    #[arg(
        long,
        requires = "x",
        allow_hyphen_values = true,
        help = "Hit-test y (logical points)"
    )]
    pub y: Option<f32>,
}

#[derive(Args, Debug)]
pub struct AgentGuiEventsArgs {
    #[arg(
        long,
        visible_alias = "kind",
        value_delimiter = ',',
        help = "Only return these event kinds (comma-separated), e.g. click,view-change"
    )]
    pub kinds: Option<Vec<String>>,
    #[arg(
        long,
        help = "Only return events after this generation (incremental tail)"
    )]
    pub since: Option<u64>,
    #[arg(
        long,
        help = "Return at most this many of the most recent matching events"
    )]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiClockArgs {
    #[arg(help = "advance | freeze | resume | status")]
    pub action: String,
    #[arg(long, help = "Milliseconds to advance (for clock advance)")]
    pub ms: Option<u64>,
}

#[derive(Args, Debug)]
pub struct AgentGuiDialogArgs {
    #[arg(help = "expect | pending | clear")]
    pub action: String,
    #[arg(long, help = "Path the next picker should return (for dialog expect)")]
    pub path: Option<PathBuf>,
    #[arg(long, help = "Cancel the next picker instead of returning a path")]
    pub cancel: bool,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("wait_condition")
        .required(true)
        .args(&[
            "text",
            "view",
            "idle",
            "modal_open",
            "modal_closed",
            "toast",
            "busy_reason_cleared",
            "download_complete",
            "download_transferring",
            "fps_above",
            "node_visible",
        ])
))]
pub struct AgentGuiWaitArgs {
    #[arg(long, help = "Wait until known UI text contains this value")]
    pub text: Option<String>,
    #[arg(long, help = "Wait until the current view matches")]
    pub view: Option<String>,
    #[arg(long, help = "Wait until the UI has no known background work")]
    pub idle: bool,
    #[arg(long, help = "Wait until at least one modal/dialog is open")]
    pub modal_open: bool,
    #[arg(long, help = "Wait until no modal/dialog is open")]
    pub modal_closed: bool,
    #[arg(
        long,
        help = "Wait until a feedback toast whose message contains this value is showing"
    )]
    pub toast: Option<String>,
    #[arg(
        long,
        help = "Wait until the named busy reason (e.g. core-sync) is no longer set"
    )]
    pub busy_reason_cleared: Option<String>,
    #[arg(long, help = "Wait until a download has completed")]
    pub download_complete: bool,
    #[arg(
        long,
        help = "Wait until the running download has received its first bytes"
    )]
    pub download_transferring: bool,
    #[arg(
        long,
        help = "Wait until the smoothed FPS estimate is at or above this value"
    )]
    pub fps_above: Option<f32>,
    #[arg(
        long,
        help = "Wait until a known semantic node with this id is on-screen"
    )]
    pub node_visible: Option<String>,
    #[arg(long, default_value_t = 5000, help = "Timeout in milliseconds")]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AgentGuiLogsArgs {
    #[arg(
        long,
        help = "Only return entries at this level (error, warn, info, debug, trace)"
    )]
    pub level: Option<String>,
    #[arg(
        long,
        help = "Only return entries whose message or source contains this value"
    )]
    pub contains: Option<String>,
    #[arg(
        long,
        help = "Return at most this many of the most recent matching entries"
    )]
    pub limit: Option<usize>,
    #[arg(
        long,
        help = "Only return entries appended after this log generation (from a prior logs call)"
    )]
    pub since_generation: Option<u64>,
}

#[derive(Args, Debug)]
pub struct AgentGuiRepositoriesArgs {
    #[arg(
        long,
        help = "Only return repositories whose name or address contains this value"
    )]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of repositories to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiAddonsArgs {
    #[arg(
        long,
        help = "Zero-based repository index (defaults to the selected repository)"
    )]
    pub repo_index: Option<usize>,
    #[arg(
        long,
        help = "Addon tab: configuration/addons, optional-addons, or external-addons (defaults to the current repository-settings tab)"
    )]
    pub tab: Option<String>,
    #[arg(long, help = "Only return addons whose name contains this value")]
    pub contains: Option<String>,
    #[arg(long, help = "Only return enabled addons")]
    pub enabled_only: bool,
    #[arg(long, help = "Maximum number of addons to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiScaleArgs {
    #[arg(help = "UI scale percent (clamped to the settings slider range, 100 = native)")]
    pub percent: u16,
}

#[derive(Args, Debug)]
pub struct AgentGuiResizeArgs {
    #[arg(long, help = "Window inner width in logical points")]
    pub width: f32,
    #[arg(long, help = "Window inner height in logical points")]
    pub height: f32,
}

#[derive(Args, Debug)]
pub struct AgentGuiProfilesArgs {
    #[arg(
        long,
        help = "Zero-based repository index (defaults to the selected repository)"
    )]
    pub repo_index: Option<usize>,
    #[arg(long, help = "Only return profiles whose name contains this value")]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of profiles to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiMissionsArgs {
    #[arg(
        long,
        help = "Only return missions whose name, folder, or terrain contains this value"
    )]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of missions to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiSpacesArgs {
    #[arg(long, help = "Only return spaces whose name contains this value")]
    pub contains: Option<String>,
    #[arg(long, help = "Maximum number of spaces to return")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AgentGuiDownloadSummaryArgs {
    #[arg(
        long,
        help = "Include the full per-sample telemetry series (default: counts only)"
    )]
    pub include_telemetry: bool,
}

#[derive(Args, Debug)]
pub struct AgentGuiSetSettingArgs {
    #[arg(
        help = "Setting key: debug-mode, show-activity-log, show-fps-counter, ui-scale-percent, locale, or download-speed-limit-mbps"
    )]
    pub key: String,
    #[arg(
        help = "New value (bool: true/false; ui-scale-percent: integer; download-speed-limit-mbps: integer or 'unlimited')"
    )]
    pub value: String,
}

#[derive(Subcommand, Debug)]
pub enum SettingsCommand {
    /// Show effective settings.
    Show,
    /// Update selected settings fields.
    Set(Box<SettingsSetArgs>),
    /// Reset settings to defaults.
    Reset,
}

#[derive(Args, Debug)]
pub struct SettingsSetArgs {
    #[arg(
        long,
        value_enum,
        help = "Save the default UI mode (server management is work in progress)"
    )]
    pub app_mode: Option<AppModeArg>,
    #[arg(long, help = "Show or hide debug windows (true/false)")]
    pub show_debug_windows: Option<bool>,
    #[arg(long, help = "Show or hide footer activity log toggle (true/false)")]
    pub show_activity_log: Option<bool>,
    #[arg(
        long,
        help = "Show or hide memory diagnostics icon in footer (true/false)"
    )]
    pub show_memory_diagnostics_icon: Option<bool>,
    #[arg(
        long,
        help = "Write extended hashing, download, database and resource diagnostics to the logs (true/false)"
    )]
    pub extended_diagnostics_logging: Option<bool>,
    #[arg(
        long,
        help = "After a database reset, restore files the verified-hash record proves unchanged instead of reading them again; an integrity recheck always reads every file (true/false)"
    )]
    pub trust_verified_hashes: Option<bool>,
    #[arg(
        long,
        help = "Offer to save user-triggered rechecks and updates as benchmarks; enabling also turns on extended diagnostics logging unless it was already on (true/false)"
    )]
    pub benchmarks_enabled: Option<bool>,
    #[arg(long, help = "Close Foxy after launching Arma (true/false)")]
    pub close_after_launch: Option<bool>,
    #[arg(long, help = "Hide Foxy to tray after launch (true/false)")]
    pub hide_to_tray_after_launch: Option<bool>,
    #[arg(
        long,
        help = "Automatically recheck repositories on startup (true/false)"
    )]
    pub auto_recheck_on_launch: Option<bool>,
    #[arg(
        long,
        help = "Refresh repository metadata while the desktop app is idle (true/false)"
    )]
    pub periodic_remote_check_enabled: Option<bool>,
    #[arg(
        long,
        help = "Minutes between idle repository metadata refreshes (5-1440)"
    )]
    pub periodic_remote_check_interval_minutes: Option<u32>,
    #[arg(
        long,
        help = "Automatically run quick local scan on startup (true/false)"
    )]
    pub auto_quick_scan_on_launch: Option<bool>,
    #[arg(
        long,
        help = "Automatically retry temporary download failures in the desktop app (true/false)"
    )]
    pub retry_failed_downloads: Option<bool>,
    #[arg(
        long,
        help = "Auto-apply launch parameters from repo.json metadata (true/false)"
    )]
    pub apply_repo_json_client_parameters: Option<bool>,
    #[arg(
        long,
        help = "Auto-apply DLC content from repo.json metadata (true/false)"
    )]
    pub apply_repo_json_dlc_content: Option<bool>,
    #[arg(
        long,
        help = "Warn before opening Eden Editor with external addons enabled (true/false)"
    )]
    pub warn_editor_external_addons: Option<bool>,
    #[arg(
        long,
        help = "Show Editor Missions list in the repository view (true/false)"
    )]
    pub enable_editor_mission_list: Option<bool>,
    #[arg(long, help = "Show Servers list in the repository view (true/false)")]
    pub enable_server_list: Option<bool>,
    #[arg(long, help = "Set Arma 3 installation directory")]
    pub arma3_dir: Option<PathBuf>,
    #[arg(long, help = "Set Total War: WARHAMMER III installation directory")]
    pub twwh3_dir: Option<PathBuf>,
    #[arg(long, help = "Set Arma Reforger installation directory")]
    pub reforger_dir: Option<PathBuf>,
    #[arg(long, help = "Set the generic game space installation directory")]
    pub generic_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Set the generic game executable, relative to its directory or absolute"
    )]
    pub generic_executable: Option<String>,
    #[arg(
        long,
        help = "Set the generic game Steam app id; 0 clears it and launches the executable directly"
    )]
    pub generic_steam_app_id: Option<u32>,
    #[arg(
        long,
        help = "Set the generic game launch argument template (tokens: {mods}, {mods_sep=;}, {mod_ids}, {manifest_name}, {extra})"
    )]
    pub generic_launch_args: Option<String>,
    #[arg(
        long,
        help = "Set the generic game mods manifest file name written into its directory"
    )]
    pub generic_mods_manifest: Option<String>,
    #[arg(long, help = "Set Arma 3 profiles directory passed as -profiles")]
    pub arma3_profiles_dir: Option<PathBuf>,
    #[arg(long, help = "Set Steam installation directory")]
    pub steam_dir: Option<PathBuf>,
    #[arg(long, help = "Set temporary working directory")]
    pub temp_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Set global download speed limit in Mbps (minimum applied value is 1)"
    )]
    pub download_speed_limit_mbps: Option<u32>,
    #[arg(long, help = "Disable global download speed limit")]
    pub download_speed_unlimited: bool,
    #[arg(
        long,
        value_parser = clap::value_parser!(u8).range(10..=100),
        help = "Most of the whole computer's processor time Foxy's work may use, in percent (10-100; default 90)"
    )]
    pub cpu_usage_limit: Option<u8>,
    #[arg(
        long,
        help = "Let Foxy's work use every processor thread when nothing else needs it"
    )]
    pub cpu_usage_unlimited: bool,
    #[arg(
        long,
        help = "Set the processor threads hashing, scanning and checking plan around (minimum 1; kept within the detected processors while safe limits are on)"
    )]
    pub cpu_threads: Option<u32>,
    #[arg(
        long,
        help = "Plan around every processor thread the system reports (default)"
    )]
    pub cpu_threads_auto: bool,
    #[arg(
        long,
        help = "Keep the CPU thread count within the detected processors; false accepts any count up to 1024 (true/false)"
    )]
    pub cpu_safe_limits: Option<bool>,
    #[arg(
        long,
        value_enum,
        help = "Processor priority of the threads that hash, scan, check and download (Windows)"
    )]
    pub work_priority: Option<WorkPriorityArg>,
    #[arg(
        long,
        help = "Run work threads in Windows background mode, which lowers their disk and memory priority (true/false)"
    )]
    pub low_disk_priority: Option<bool>,
    #[arg(
        long,
        value_enum,
        help = "Storage type assumed for every drive instead of the detected one"
    )]
    pub storage_type: Option<StorageTypeArg>,
    #[arg(
        long,
        value_enum,
        help = "Memory pressure level assumed instead of the measured one"
    )]
    pub memory_pressure: Option<MemoryPressureArg>,
    #[arg(long, value_enum, help = "How HTTP requests reach the network")]
    pub proxy_mode: Option<ProxyModeArg>,
    #[arg(
        long,
        help = "Set the manual proxy address, for example http://proxy.example:8080; credentials go in --proxy-username and --proxy-password, and an empty value clears it"
    )]
    pub proxy_url: Option<String>,
    #[arg(
        long,
        help = "Set the manual proxy user name; an empty value clears it"
    )]
    pub proxy_username: Option<String>,
    #[arg(
        long,
        help = "Set the manual proxy password, stored as plain text; other local programs can read command lines, so prefer the Settings view"
    )]
    pub proxy_password: Option<String>,
    #[arg(
        long,
        help = "Comma-separated hosts, domains or IP ranges reached without the proxy"
    )]
    pub proxy_bypass: Option<String>,
    #[arg(long, help = "Set UI locale (for example: en, cs, system)")]
    pub locale: Option<String>,
    #[arg(long, help = "Add additional addon search folder (repeatable)")]
    pub add_additional_folder: Vec<PathBuf>,
    #[arg(long, help = "Remove additional addon search folder (repeatable)")]
    pub remove_additional_folder: Vec<PathBuf>,
    #[arg(long, help = "Set additional folder alias as PATH=ALIAS (repeatable)")]
    pub set_additional_folder_alias: Vec<String>,
    #[arg(long, help = "Clear additional folder alias for PATH (repeatable)")]
    pub clear_additional_folder_alias: Vec<PathBuf>,
    #[arg(long, help = "Add cleanup folder (repeatable)")]
    pub add_cleanup_folder: Vec<PathBuf>,
    #[arg(long, help = "Remove cleanup folder (repeatable)")]
    pub remove_cleanup_folder: Vec<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum RepoCommand {
    /// List repositories from config.
    List,
    /// Add repository by URL and optional local path/name.
    Add(RepoAddArgs),
    /// Remove repository from config.
    Remove(RepoRemoveArgs),
    /// Clone repository entry with name suffix.
    Clone(RepoCloneArgs),
    /// Sync one repository using selected mode.
    Sync(RepoSyncArgs),
    /// Wipe cached database metadata for one repository.
    WipeDb(RepoWipeDbArgs),
    /// Remove local files and force full re-download.
    ForceRedownload(RepoForceRedownloadArgs),
}

#[derive(Args, Debug)]
pub struct RepoAddArgs {
    #[arg(long, help = "Repository URL or root address")]
    pub address: String,
    #[arg(long, help = "Optional display name")]
    pub name: Option<String>,
    #[arg(long, help = "Optional local destination folder")]
    pub path: Option<PathBuf>,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("selector")
        .required(true)
        .args(&["repo_name", "repo_url"])
))]
pub struct RepoSelectorArgs {
    #[arg(
        long,
        help = "Select repository by display name (must be unique, case-insensitive match)"
    )]
    pub repo_name: Option<String>,
    #[arg(
        long,
        help = "Select repository by URL (normalized with trailing slash)"
    )]
    pub repo_url: Option<String>,
}

#[derive(Args, Debug)]
pub struct RepoRemoveArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
}

#[derive(Args, Debug)]
pub struct RepoCloneArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Suffix appended to cloned repository name")]
    pub suffix: String,
}

#[derive(Args, Debug)]
pub struct RepoSyncArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Synchronization mode")]
    pub mode: RepoSyncMode,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum WorkPriorityArg {
    #[value(help = "Compete with other programs as usual")]
    Normal,
    #[value(help = "Let programs at normal priority, such as a running game, go first")]
    BelowNormal,
    #[value(help = "Run only when the processor has nothing else to do")]
    Lowest,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum StorageTypeArg {
    #[value(help = "Detect each drive's type")]
    Auto,
    Ssd,
    Hdd,
    Removable,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum MemoryPressureArg {
    #[value(help = "Measure free memory before each check and download")]
    Auto,
    Normal,
    Constrained,
    Severe,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ProxyModeArg {
    #[value(help = "Windows proxy settings and the HTTP_PROXY/HTTPS_PROXY variables")]
    System,
    #[value(help = "Connect directly")]
    None,
    #[value(help = "The proxy set with --proxy-url")]
    Manual,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum RepoSyncMode {
    #[value(help = "Fetch remote metadata and refresh update plan")]
    RemoteRefresh,
    #[value(help = "Run quick local drift check using content hashes")]
    QuickCheck,
    #[value(help = "Run repository recheck (tree/hash verification path)")]
    Recheck,
    #[value(help = "Recheck repository integrity (full remote fetch + local hash recalculation)")]
    RecheckIntegrity,
    #[value(help = "Download and apply updates")]
    Download,
}

#[derive(Args, Debug)]
pub struct RepoWipeDbArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(
        long,
        help = "Keep the folder's verified-hash record, so the next check may restore unchanged files instead of reading them"
    )]
    pub keep_hash_record: bool,
}

#[derive(Args, Debug)]
pub struct RepoForceRedownloadArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
}

#[derive(Subcommand, Debug)]
pub enum AddonCommand {
    /// List addons for the effective repository/profile.
    List(AddonListArgs),
    /// Enable or disable one addon.
    Set(AddonSetArgs),
    /// Recheck integrity for one addon (recalculate hashes then verify repository state).
    RecalcHashes(AddonRecalcHashesArgs),
    /// Delete local addon folder and trigger re-download.
    ForceRedownload(AddonForceRedownloadArgs),
}

#[derive(Args, Debug)]
pub struct AddonListArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
}

#[derive(Args, Debug)]
pub struct AddonSetArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Addon name")]
    pub addon: String,
    #[arg(long, help = "Target enabled state (true/false)")]
    pub enabled: bool,
}

#[derive(Args, Debug)]
pub struct AddonRecalcHashesArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Addon name")]
    pub addon: String,
}

#[derive(Args, Debug)]
pub struct AddonForceRedownloadArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Addon name")]
    pub addon: String,
}

#[derive(Subcommand, Debug)]
pub enum ProfileCommand {
    /// List profiles for one repository.
    List(ProfileListArgs),
    /// Select active profile for one repository (`default` clears selection).
    Select(ProfileSelectArgs),
    /// Add a new profile.
    Add(ProfileAddArgs),
    /// Delete profile by name.
    Delete(ProfileDeleteArgs),
}

#[derive(Args, Debug)]
pub struct ProfileListArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
}

#[derive(Args, Debug)]
pub struct ProfileSelectArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Profile name (use `default` to clear selection)")]
    pub profile: String,
}

#[derive(Args, Debug)]
pub struct ProfileAddArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "New profile name")]
    pub profile: String,
}

#[derive(Args, Debug)]
pub struct ProfileDeleteArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Profile name")]
    pub profile: String,
}

#[derive(Subcommand, Debug)]
pub enum SpaceCommand {
    /// List repository spaces from config.
    List,
    /// Sync all repositories attached to one repository space.
    Sync(SpaceSyncArgs),
}

#[derive(Subcommand, Debug)]
pub enum GameCommand {
    /// List game spaces and which one is active.
    List,
    /// Set the active game space (loads on next UI start; CLI commands use it immediately).
    Use(GameUseArgs),
    /// Create a new game space for a registered game.
    Create(GameCreateArgs),
    /// Remove a game space's Foxy workspace (requires --yes).
    Remove(GameRemoveArgs),
    /// Build or execute the active game-space launcher without a repository.
    Launch(GameLaunchArgs),
    /// Manage Arma Reforger Workshop GUID folders for the active game space.
    Reforger {
        #[command(subcommand)]
        command: ReforgerCommand,
    },
}

#[derive(Args, Debug)]
pub struct GameUseArgs {
    /// Game space id (see `foxy game list`).
    pub space_id: String,
}

#[derive(Args, Debug)]
pub struct GameCreateArgs {
    /// Display name of the new game space.
    pub name: String,
    #[arg(
        long,
        default_value = "arma3",
        help = "Game module id for the new space"
    )]
    pub game: String,
}

#[derive(Args, Debug)]
pub struct GameRemoveArgs {
    /// Game space id (see `foxy game list`).
    pub space_id: String,
    #[arg(
        long,
        help = "Preserve private frozen mods and extra-file payloads before removal"
    )]
    pub preserve_payloads: bool,
    #[arg(
        long,
        conflicts_with = "preserve_payloads",
        help = "Explicitly discard private frozen mods and extra-file payloads"
    )]
    pub discard_payloads: bool,
}

#[derive(Args, Debug)]
pub struct GameLaunchArgs {
    #[arg(long, help = "Keep the changed destination for this extra-file id")]
    pub keep_extra_file: Vec<String>,
    #[arg(long, help = "Reapply the stored template for this extra-file id")]
    pub reapply_extra_file: Vec<String>,
    #[arg(long, help = "Approve the displayed ordered extra-file replacements")]
    pub approve_extra_file_layers: bool,
    #[arg(
        long,
        help = "Execute launch immediately (without this flag, command prints launch spec)"
    )]
    pub execute: bool,
    #[arg(long, help = "Include disabled Workshop items in the launch manifest")]
    pub include_disabled: bool,
    #[arg(long, help = "Optional TW:WH3 save name for continue-campaign launch")]
    pub save_name: Option<String>,
    #[arg(
        long,
        help = "Allow this launch to continue if extra files cannot be applied"
    )]
    pub allow_incomplete_preparation: bool,
    #[arg(
        long,
        help = "Allow this launch to use live Workshop files when a frozen copy is missing"
    )]
    pub allow_live_fallback: bool,
}

#[derive(Subcommand, Debug)]
pub enum ReforgerCommand {
    /// List managed Arma Reforger addons.
    List,
    /// Register one GUID folder, optionally copying it into Foxy's managed store.
    Add(ReforgerAddArgs),
    /// Import many GUIDs from text, URLs, or a file.
    Import(ReforgerImportArgs),
    /// Remove one managed GUID entry.
    Remove(ReforgerRemoveArgs),
    /// Enable or disable one managed GUID.
    Set(ReforgerSetArgs),
    /// Freeze one present GUID folder into a Foxy-managed snapshot.
    Freeze(ReforgerFreezeArgs),
    /// Resume launching from the live GUID folder.
    Unfreeze(ReforgerUnfreezeArgs),
    /// Export managed GUIDs.
    Export(ReforgerExportArgs),
    /// Resolve the path Foxy will launch for one GUID.
    Resolve(ReforgerResolveArgs),
}

#[derive(Args, Debug)]
pub struct ReforgerAddArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
    #[arg(long, help = "Display name stored in reforger_addons.json")]
    pub name: Option<String>,
    #[arg(long, help = "Existing GUID folder to copy into Foxy's managed store")]
    pub source: Option<PathBuf>,
    #[arg(long, help = "Add the GUID disabled")]
    pub disabled: bool,
}

#[derive(Args, Debug)]
pub struct ReforgerImportArgs {
    /// Reforger Workshop GUIDs or URLs. Use quotes for a pasted multiline list.
    pub input: Vec<String>,
    #[arg(long, help = "Read additional GUIDs or URLs from a text file")]
    pub from_file: Option<PathBuf>,
    #[arg(long, help = "Folder containing one child directory per GUID")]
    pub source_root: Option<PathBuf>,
    #[arg(long, help = "Import entries disabled")]
    pub disabled: bool,
}

#[derive(Args, Debug)]
pub struct ReforgerRemoveArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
    #[arg(long, help = "Delete Foxy's managed live and frozen copies")]
    pub delete_data: bool,
}

#[derive(Args, Debug)]
pub struct ReforgerSetArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
    #[arg(long, help = "Target enabled state (true/false)")]
    pub enabled: bool,
}

#[derive(Args, Debug)]
pub struct ReforgerFreezeArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
}

#[derive(Args, Debug)]
pub struct ReforgerUnfreezeArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
}

#[derive(Args, Debug)]
pub struct ReforgerExportArgs {
    #[arg(long, help = "Include disabled items")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct ReforgerResolveArgs {
    /// Reforger Workshop GUID or URL.
    pub guid: String,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Export the active game space to a .foxypack archive.
    Export(ConfigExportArgs),
    /// Import a .foxypack archive into the active game space.
    Import(ConfigImportArgs),
    /// Manage active game-space extra files.
    ExtraFile {
        #[command(subcommand)]
        command: ConfigExtraFileCommand,
    },
}

#[derive(Args, Debug)]
pub struct ConfigExportArgs {
    /// Destination .foxypack path.
    pub output: PathBuf,
}

#[derive(Args, Debug)]
pub struct ConfigImportArgs {
    /// Source .foxypack path.
    pub input: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum ConfigExtraFileCommand {
    /// List managed extra files.
    List,
    /// Add a file or folder to the managed extra-file store.
    Add(ConfigExtraFileAddArgs),
    /// Remove a managed extra file by id.
    Remove(ConfigExtraFileRemoveArgs),
    /// Enable or disable one managed extra file.
    Set(ConfigExtraFileSetArgs),
    /// Copy enabled extra files to their configured destinations.
    Activate(ConfigExtraFileActivateArgs),
}

#[derive(Args, Debug, Default)]
pub struct ConfigExtraFileActivateArgs {
    #[arg(long, help = "Keep the changed destination for this extra-file id")]
    pub keep_extra_file: Vec<String>,
    #[arg(long, help = "Reapply the stored template for this extra-file id")]
    pub reapply_extra_file: Vec<String>,
    #[arg(long, help = "Approve the displayed ordered extra-file replacements")]
    pub approve_extra_file_layers: bool,
}

#[derive(Args, Debug)]
pub struct ConfigExtraFileAddArgs {
    #[arg(long, help = "Insert at this zero-based layer position")]
    pub order: Option<usize>,
    #[arg(long, help = "Display name")]
    pub name: String,
    #[arg(long, help = "Source file or folder to copy into the Foxy store")]
    pub source: PathBuf,
    #[arg(
        long,
        help = "Destination path, absolute or using {game_dir} as the game install directory"
    )]
    pub destination: String,
    #[arg(long, help = "Add the entry disabled")]
    pub disabled: bool,
}

#[derive(Args, Debug)]
pub struct ConfigExtraFileRemoveArgs {
    /// Extra-file id from `foxy config extra-file list`.
    pub id: String,
}

#[derive(Args, Debug)]
pub struct ConfigExtraFileSetArgs {
    #[arg(long, help = "Move to this zero-based layer position")]
    pub order: Option<usize>,
    /// Extra-file id from `foxy config extra-file list`.
    pub id: String,
    #[arg(long, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true", help = "Target enabled state (true/false)")]
    pub enabled: Option<bool>,
}

#[derive(Subcommand, Debug)]
pub enum WorkshopCommand {
    /// List managed Steam Workshop items.
    List,
    /// Add one Steam Workshop item by id or URL.
    Add(WorkshopAddArgs),
    /// Import many Steam Workshop item ids or URLs.
    Import(WorkshopImportArgs),
    /// Remove one managed Steam Workshop item.
    Remove(WorkshopRemoveArgs),
    /// Unsubscribe through Steam, optionally deleting its shared live copy.
    Unsubscribe(WorkshopRemoveArgs),
    /// Enable or disable one managed Steam Workshop item.
    Set(WorkshopSetArgs),
    /// Freeze installed Workshop items into Foxy's managed snapshot store.
    Freeze(WorkshopFreezeArgs),
    /// Report which managed items are pinned, in sync, or drifted from Steam.
    Pins(WorkshopPinsArgs),
    /// Resume launching from Steam's live Workshop folder.
    Unfreeze(WorkshopUnfreezeArgs),
    /// Export managed Workshop ids or URLs.
    Export(WorkshopExportArgs),
    /// Print the pipe-separated share code for the managed Workshop items.
    Share(WorkshopShareArgs),
    /// Set or clear the launch order position of one managed item.
    Order(WorkshopOrderArgs),
    /// Print the shareable state checksum of the active game space.
    Checksum(WorkshopChecksumArgs),
    /// Export or import a .foxyshare bundle of the managed Workshop items.
    Bundle {
        #[command(subcommand)]
        command: WorkshopBundleCommand,
    },
    /// Resolve the path Foxy will launch for one Workshop item.
    Resolve(WorkshopResolveArgs),
}

#[derive(Subcommand, Debug)]
pub enum WorkshopBundleCommand {
    /// Write a .foxyshare bundle for the managed Workshop items.
    Export(WorkshopBundleExportArgs),
    /// Read a .foxyshare bundle without changing anything.
    Inspect(WorkshopBundleInspectArgs),
    /// Import a .foxyshare bundle into the active game space.
    Import(WorkshopBundleImportArgs),
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkshopDownloadBackend {
    SteamHelper,
    Steamcmd,
    None,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkshopExportFormat {
    Ids,
    Urls,
}

#[derive(Args, Debug)]
pub struct WorkshopAddArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
    #[arg(long, help = "Override title stored in workshop.json")]
    pub name: Option<String>,
    #[arg(long, help = "Add the item disabled")]
    pub disabled: bool,
    #[arg(
        long,
        help = "Freeze the item after downloading so its version is pinned"
    )]
    pub freeze: bool,
    #[command(flatten)]
    pub download: WorkshopDownloadArgs,
}

#[derive(Args, Debug)]
pub struct WorkshopImportArgs {
    /// Steam Workshop ids or URLs. Use quotes for a pasted multiline list.
    pub input: Vec<String>,
    #[arg(long, help = "Read additional ids or URLs from a text file")]
    pub from_file: Option<PathBuf>,
    #[arg(
        long,
        help = "Treat input ids as Steam collection ids and import their children"
    )]
    pub collection: bool,
    #[arg(long, help = "Import entries disabled")]
    pub disabled: bool,
    #[arg(
        long,
        help = "Freeze each item after downloading so its version is pinned"
    )]
    pub freeze: bool,
    #[command(flatten)]
    pub download: WorkshopDownloadArgs,
}

#[derive(Args, Debug)]
pub struct WorkshopDownloadArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = WorkshopDownloadBackend::SteamHelper,
        help = "Download backend: steam-helper, steamcmd, or none"
    )]
    pub backend: WorkshopDownloadBackend,
    #[arg(long, help = "Alias for --backend none")]
    pub skip_download: bool,
    #[arg(long, help = "Do not call Steam Web API metadata endpoints")]
    pub skip_metadata: bool,
    #[arg(long, default_value_t = 300, help = "Steam helper timeout in seconds")]
    pub timeout_seconds: u64,
    #[arg(
        long,
        help = "Path to steamcmd executable when --backend steamcmd is used"
    )]
    pub steamcmd: Option<PathBuf>,
    #[arg(long, help = "SteamCMD login user; defaults to anonymous")]
    pub steamcmd_user: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkshopRemoveArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
    #[arg(
        long,
        help = "Delete private frozen copies on remove, or shared Steam content on unsubscribe"
    )]
    pub delete_data: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = WorkshopDownloadBackend::SteamHelper,
        help = "Steam backend used only by unsubscribe: steam-helper or none"
    )]
    pub backend: WorkshopDownloadBackend,
    #[arg(long, default_value_t = 300, help = "Steam helper timeout in seconds")]
    pub timeout_seconds: u64,
}

#[derive(Args, Debug)]
pub struct WorkshopSetArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
    #[arg(long, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true", default_value_t = false, help = "Target enabled state (true/false)")]
    pub enabled: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopFreezeArgs {
    /// Steam Workshop item id or URL. Omit it with --all.
    pub item: Option<String>,
    #[arg(long, help = "Freeze every managed item of the active game")]
    pub all: bool,
    #[arg(long, help = "Re-freeze items that are already pinned")]
    pub refresh: bool,
    #[arg(long, help = "With --all, include disabled items")]
    pub include_disabled: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopPinsArgs {
    #[arg(long, help = "List only items whose pin has drifted from Steam")]
    pub drifted_only: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopUnfreezeArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
}

#[derive(Args, Debug)]
pub struct WorkshopExportArgs {
    #[arg(long, value_enum, default_value_t = WorkshopExportFormat::Urls)]
    pub format: WorkshopExportFormat,
    #[arg(long, help = "Include disabled items")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopShareArgs {
    #[arg(long, help = "Include disabled items")]
    pub all: bool,
    #[arg(long, help = "Append ;<position> load order to every entry")]
    pub load_order: bool,
    #[arg(
        long,
        help = "Append @<version> pins; Foxy reads these, other mod managers do not"
    )]
    pub versions: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopOrderArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
    #[arg(long, help = "Launch order position; lower loads first")]
    pub position: Option<u32>,
    #[arg(long, help = "Clear the stored position")]
    pub clear: bool,
}

#[derive(Args, Debug)]
pub struct WorkshopChecksumArgs {
    #[arg(
        long,
        value_name = "FILE",
        help = "Compare against a checksum JSON file written by another player"
    )]
    pub compare: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct WorkshopBundleExportArgs {
    /// Destination .foxyshare file.
    pub output: PathBuf,
    #[arg(long, help = "Include disabled items")]
    pub all: bool,
    #[arg(long, help = "List frozen items without copying their files")]
    pub no_payloads: bool,
    #[arg(long, help = "Note stored in the bundle manifest")]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkshopBundleInspectArgs {
    /// Bundle to read.
    pub input: PathBuf,
}

#[derive(Args, Debug)]
pub struct WorkshopBundleImportArgs {
    /// Bundle to import.
    pub input: PathBuf,
    #[arg(
        long,
        value_enum,
        help = "Merge incoming items or match the bundle's enabled selection"
    )]
    pub mode: Option<WorkshopBundleMode>,
    #[arg(long, help = "Record entries without restoring frozen payloads")]
    pub skip_payloads: bool,
    #[arg(
        long,
        help = "Freeze items the bundle did not carry files for, after downloading them"
    )]
    pub freeze: bool,
    #[command(flatten)]
    pub download: WorkshopDownloadArgs,
}

#[derive(Args, Debug)]
pub struct WorkshopResolveArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkshopBundleMode {
    Merge,
    Match,
}

#[derive(Subcommand, Debug)]
pub enum SteamHelperCommand {
    /// Subscribe and download one Steam Workshop item through Steamworks.
    Install(SteamHelperItemArgs),
    /// Unsubscribe one Steam Workshop item through Steamworks.
    Remove(SteamHelperItemArgs),
    /// Inspect one Steam Workshop item through Steamworks.
    Status(SteamHelperItemArgs),
}

#[derive(Args, Debug)]
pub struct SteamHelperItemArgs {
    #[arg(long)]
    pub app_id: u32,
    #[arg(long)]
    pub item_id: String,
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("space_sync_operation")
        .required(false)
        .args(&["mode", "recheck_all", "update_all"])
))]
pub struct SpaceSyncArgs {
    #[arg(long, help = "Repository space ID")]
    pub space_id: String,
    #[arg(
        long,
        help = "Synchronization mode for each attached repository (remote-refresh or quick-check)"
    )]
    pub mode: Option<SpaceSyncMode>,
    #[arg(
        long,
        help = "Recheck all repositories in the selected space (same as --mode remote-refresh)"
    )]
    pub recheck_all: bool,
    #[arg(
        long,
        help = "Download updates for repositories in the selected space that currently have pending updates"
    )]
    pub update_all: bool,
    #[arg(
        long,
        value_name = "NAME_OR_INDEX",
        value_delimiter = ',',
        help = "Include only repositories matching name or 1-based index (repeatable or comma-separated)"
    )]
    pub select: Vec<String>,
    #[arg(
        long,
        value_name = "NAME_OR_INDEX",
        value_delimiter = ',',
        help = "Exclude repositories matching name or 1-based index (repeatable or comma-separated)"
    )]
    pub exclude: Vec<String>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum SpaceSyncMode {
    #[value(help = "Fetch remote metadata for each attached repository")]
    RemoteRefresh,
    #[value(help = "Run quick local drift check for each attached repository")]
    QuickCheck,
}

#[derive(Subcommand, Debug)]
pub enum ServerCommand {
    /// Review and acquire an exact Reforger addon set in isolated native engine roots.
    ReforgerAddons {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerReforgerAddonsCommand,
    },
    /// Prepare and verify protected native Steam sessions without opening the game database.
    Steam {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerSteamCommand,
    },
    #[command(hide = true)]
    ConsoleSignal { handle: u64 },
    #[command(hide = true)]
    CleanupWitness {
        owner: u64,
        tree: u64,
        receipt: u64,
        ready: u64,
    },
    /// Host one foreground owner until a shutdown signal or authenticated idle shutdown.
    Supervise(ServerSuperviseArgs),
    /// Export a user service definition without registering or starting it.
    Service {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerServiceCommand,
    },
    /// Review, publish or verify player content from the selected committed deployment.
    Publish {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerPublishCommand,
    },
    /// Authenticate and inspect the current foreground owner without opening Turso.
    Owner(ServerOwnerArgs),
    /// Inspect a captured game-space context without creating files or opening its database.
    Context(ServerContextArgs),
    /// Report implemented server operations for an existing game space.
    Capabilities(ServerContextArgs),
    /// Inspect and edit saved server drafts without installing or launching a server.
    Instance {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerInstanceCommand,
    },
    /// Define, review and start Arma headless clients for a server draft.
    Headless {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerHeadlessCommand,
    },
    /// Define scheduled maintenance jobs, run or cancel them, and review their occurrences.
    Maintenance {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerMaintenanceCommand,
    },
    /// Plan, create, verify, pin and safely prune bounded server backups.
    Backup {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerBackupCommand,
    },
    /// Review and transactionally restore compatible local server saves.
    Restore {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerRestoreCommand,
    },
    /// Inspect Reforger saves and review the next launch's session selection.
    Session {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerSessionCommand,
    },
    /// Manage protected RCon credentials, check the owner's RCon session and send commands.
    Rcon {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerRconCommand,
    },
    /// Send typed Arma 3 administration commands through an authenticated running owner.
    Admin {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerArmaAdminCommand,
    },
    /// Send typed Reforger administration commands through the authenticated owner.
    ReforgerAdmin {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerReforgerAdminCommand,
    },
    /// Review, acquire and verify Arma Workshop items as pinned server content.
    Workshop {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerWorkshopCommand,
    },
    /// Inspect existing native installations without running or modifying them.
    Installation {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerInstallationCommand,
    },
    /// Review, save and verify configuration-only snapshots without a game process.
    Configuration {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerConfigurationCommand,
    },
    /// Collect and explicitly review public signing-key trust for a server draft.
    Keys {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerKeyCommand,
    },
    /// Import and review a private mission library and ordered instance cycles.
    Mission {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerMissionCommand,
    },
    /// Query public server addon metadata through Steam server rules.
    InspectAddons(ServerInspectAddonsArgs),
    /// Set addon folders and repository profiles without opening the sync database.
    Content {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerContentCommand,
    },
    /// Review verified configuration and content without selecting or activating a deployment.
    Deploy {
        #[command(flatten)]
        context: ServerContextArgs,
        #[command(subcommand)]
        command: ServerDeployCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerHeadlessCommand {
    /// List the draft's headless client definitions in start order.
    List(ServerInstanceSelector),
    /// Add a headless client definition to the draft.
    Add(ServerHeadlessAddArgs),
    /// Replace one headless client definition, keeping its id; omitted options reset to defaults.
    Set(ServerHeadlessSetArgs),
    /// Remove one headless client definition from the draft; requires --yes.
    Remove(ServerHeadlessRemoveArgs),
    /// Review the committed runtime's headless launch plans without starting anything.
    Plan(ServerInstanceSelector),
    /// Show each headless client of the running server as the owner observes it.
    Status(ServerInstanceSelector),
    /// Start the headless clients, or one with --client, of a running, query-ready server.
    Start(ServerHeadlessStartArgs),
    /// Stop the headless clients, or one with --client; they stay stopped until started again.
    Stop(ServerHeadlessStopArgs),
    /// Stop and start again the headless clients, or one with --client, with a fresh restart budget.
    Restart(ServerHeadlessRestartArgs),
}

#[derive(Args, Debug)]
pub struct ServerHeadlessDefinitionArgs {
    /// Unique profile name, 1-24 letters, digits, underscores or hyphens.
    #[arg(long)]
    pub name: String,
    /// Start order; ties start in id order.
    #[arg(long, default_value_t = 0)]
    pub order: u32,
    /// Keep the definition but leave it out of committed runtimes.
    #[arg(long)]
    pub disabled: bool,
    /// Do not inherit the server's required addons.
    #[arg(long)]
    pub no_inherit_required: bool,
    /// Also load this bound required or server-only content source. Repeatable.
    #[arg(long = "include")]
    pub include: Vec<String>,
    /// Skip this inherited required content source. Repeatable.
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,
    /// Engine -cpuCount, 1-256.
    #[arg(long)]
    pub cpu_count: Option<u16>,
    /// Engine -exThreads: 0, 1, 3, 5 or 7.
    #[arg(long)]
    pub ex_threads: Option<u8>,
    /// Engine -maxMem in megabytes, 256-262144.
    #[arg(long)]
    pub max_mem: Option<u32>,
    /// Engine -limitFPS, 5-1000.
    #[arg(long)]
    pub limit_fps: Option<u16>,
    /// Do not start this client automatically once the server is query-ready.
    #[arg(long)]
    pub manual_start: bool,
    /// Seconds to wait after the server first answers its query port, 0-600.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u16).range(0..=600))]
    pub start_delay: u16,
    /// Seconds after start before missing join evidence is reported, 15-900.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u16).range(15..=900))]
    pub join_timeout: u16,
    /// Automatic restarts after unexpected exits within 10 minutes, 0-10; 0 disables them.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=10))]
    pub max_restarts: u8,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessAddArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[command(flatten)]
    pub definition: ServerHeadlessDefinitionArgs,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessSetArgs {
    pub id: String,
    pub client_id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[command(flatten)]
    pub definition: ServerHeadlessDefinitionArgs,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessRemoveArgs {
    pub id: String,
    pub client_id: String,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessStartArgs {
    pub id: String,
    /// Run generation from server instance status.
    #[arg(long)]
    pub expected_run_generation: u64,
    /// Only this headless client id.
    #[arg(long)]
    pub client: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessStopArgs {
    pub id: String,
    /// Run generation from server instance status.
    #[arg(long)]
    pub expected_run_generation: u64,
    /// Only this headless client id.
    #[arg(long)]
    pub client: Option<String>,
    /// Terminate the client process trees without a graceful stop.
    #[arg(long)]
    pub force: bool,
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u32).range(1..=120_000))]
    pub timeout_ms: u32,
}

#[derive(Subcommand, Debug)]
pub enum ServerMaintenanceCommand {
    /// List the instance's jobs with next run, last result and blocker.
    List(ServerInstanceSelector),
    /// Add a maintenance job.
    Add(ServerMaintenanceAddArgs),
    /// Replace one job definition, keeping its id; a real change advances its policy revision.
    Set(ServerMaintenanceSetArgs),
    /// Remove one job; its recorded occurrences stay in history. Requires --yes.
    Remove(ServerMaintenanceRemoveArgs),
    /// Run one job now through the running owner. Requires --yes.
    Run(ServerMaintenanceRunArgs),
    /// Cancel an open occurrence. Requires --yes.
    Cancel(ServerMaintenanceCancelArgs),
    /// Show recorded occurrences, newest first.
    History(ServerInstanceSelector),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ServerMaintenanceActionArg {
    Restart,
    Update,
    Backup,
    RotateMission,
    ReloadScenario,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceDefinitionArgs {
    /// Job name, 1-48 characters.
    #[arg(long)]
    pub name: String,
    #[arg(long, value_enum)]
    pub action: ServerMaintenanceActionArg,
    /// One-time run: RFC 3339 with offset, or YYYY-MM-DDTHH:MM in the job clock.
    #[arg(long, conflicts_with_all = ["weekly", "time"])]
    pub at: Option<String>,
    /// Weekly run on these days: daily, or a comma list such as mon,wed,fri.
    #[arg(long, requires = "time")]
    pub weekly: Option<String>,
    /// Time of day HH:MM for a weekly run.
    #[arg(long, requires = "weekly")]
    pub time: Option<String>,
    /// Interpret the schedule in UTC instead of this computer's local time.
    #[arg(long)]
    pub utc: bool,
    /// Save the job without scheduling it.
    #[arg(long)]
    pub disabled: bool,
    /// Minutes a missed time may still run late, 0-1440.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(0..=1440))]
    pub grace_minutes: u16,
    /// Run only when the server reports no connected players.
    #[arg(long)]
    pub require_empty: bool,
    /// Minutes to wait for the job's conditions before skipping, 0-720.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(0..=720))]
    pub max_defer_minutes: u16,
    /// Attempts per occurrence, 1-5.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub max_attempts: u8,
    /// Seconds between attempts, 10-3600.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(10..=3600))]
    pub retry_delay_seconds: u16,
    /// Graceful stop timeout for a restart, 5-120 seconds.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(5..=120))]
    pub stop_timeout_seconds: u16,
    /// Arma only: announce before rotating missions.
    #[arg(long)]
    pub announcement: Option<String>,
    /// Delay after the announcement before mission rotation, 0-300 seconds.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u16).range(0..=300))]
    pub announcement_delay_seconds: u16,
    /// Arma only: mission template to load after rotating.
    #[arg(long)]
    pub mission_template: Option<String>,
    /// Require a successful backup before rotating missions or restarting.
    #[arg(long)]
    pub require_backup: bool,
    /// Allow best-effort live backup while the server is running (default requires stopped consistency).
    #[arg(long, conflicts_with = "stopped_backup")]
    pub live_backup: bool,
    /// Require witnessed stopped consistency for backups.
    #[arg(long, conflicts_with = "live_backup")]
    pub stopped_backup: bool,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceAddArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[command(flatten)]
    pub definition: ServerMaintenanceDefinitionArgs,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceSetArgs {
    pub id: String,
    pub job_id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[command(flatten)]
    pub definition: ServerMaintenanceDefinitionArgs,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceRemoveArgs {
    pub id: String,
    pub job_id: String,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceRunArgs {
    pub id: String,
    pub job_id: String,
    /// The job's current policy revision from list.
    #[arg(long)]
    pub expected_policy_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerMaintenanceCancelArgs {
    pub id: String,
    pub occurrence_id: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerBackupCommand {
    /// Preview backup sets, consistency witness, secret exclusions and plan fingerprint.
    Plan(ServerBackupPlanArgs),
    /// Create a bounded verified backup archive (stopped consistency by default).
    Create(ServerBackupCreateArgs),
    /// List existing server backups, retention policy and corrupt/unknown counts.
    List(ServerInstanceSelector),
    /// Stream and verify every file checksum inside a backup archive.
    Verify(ServerBackupVerifyArgs),
    /// Pin an existing verified backup so automatic or manual pruning retains it.
    Pin(ServerBackupPinArgs),
    /// Unpin a backup under its expected retention generation.
    Unpin(ServerBackupPinArgs),
    /// Update per-instance backup retention limits under an expected generation.
    Retention(ServerBackupRetentionArgs),
    /// Preview which old backups would be pruned while keeping pinned and latest stopped snapshots.
    PrunePlan(ServerBackupPrunePlanArgs),
    /// Remove unpinned excess backups matching an exact reviewed prune plan. Requires --yes.
    Prune(ServerBackupPruneArgs),
}

#[derive(Subcommand, Debug)]
pub enum ServerRestoreCommand {
    /// Verify the full archive, compatibility, current saves and disk budget without writes.
    Plan(ServerRestorePlanArgs),
    /// Restore a reviewed compatible backup. Requires --yes.
    Apply(ServerRestoreApplyArgs),
    /// Inspect a pending restore and its exact rollback fingerprint.
    Status(ServerInstanceSelector),
    /// Restore preserved pre-restore bytes after an interrupted operation. Requires --yes.
    Rollback(ServerRestoreRollbackArgs),
}

#[derive(Subcommand, Debug)]
pub enum ServerSessionCommand {
    /// Inspect native save metadata and compatibility without changing files.
    List(ServerInstanceSelector),
    /// Review configured, latest, selected UUID or new session intent.
    Plan(ServerSessionPlanArgs),
    /// Save a reviewed next-launch intent while all writers are stopped. Requires --yes.
    Apply(ServerSessionApplyArgs),
}

#[derive(Args, Debug)]
pub struct ServerSessionPlanArgs {
    pub id: String,
    #[arg(long, value_parser = ["configured", "latest", "selected", "new"])]
    pub mode: String,
    #[arg(long)]
    pub save_id: Option<String>,
    #[arg(long)]
    pub expected_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerSessionApplyArgs {
    pub id: String,
    #[arg(long, value_parser = ["configured", "latest", "selected", "new"])]
    pub mode: String,
    #[arg(long)]
    pub save_id: Option<String>,
    #[arg(long)]
    pub expected_generation: u64,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerRestorePlanArgs {
    pub id: String,
    #[arg(long)]
    pub backup_id: String,
}

#[derive(Args, Debug)]
pub struct ServerRestoreApplyArgs {
    pub id: String,
    #[arg(long)]
    pub backup_id: String,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerRestoreRollbackArgs {
    pub id: String,
    #[arg(long)]
    pub expected_journal_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerBackupPlanArgs {
    pub id: String,
    /// Plan an explicitly labeled best-effort live backup while the server runs.
    #[arg(long)]
    pub live: bool,
}

#[derive(Args, Debug)]
pub struct ServerBackupCreateArgs {
    pub id: String,
    /// Create an explicitly labeled best-effort live backup while the server runs.
    #[arg(long)]
    pub live: bool,
    /// Require the current backup plan fingerprint to match this reviewed value.
    #[arg(long)]
    pub expected_plan_fingerprint: Option<String>,
    /// Skip automatic retention pruning after the new backup is verified.
    #[arg(long)]
    pub no_retention: bool,
}

#[derive(Args, Debug)]
pub struct ServerBackupVerifyArgs {
    pub id: String,
    pub backup_id: String,
}

#[derive(Args, Debug)]
pub struct ServerBackupPinArgs {
    pub id: String,
    pub backup_id: String,
    #[arg(long)]
    pub expected_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerBackupRetentionArgs {
    pub id: String,
    #[arg(long)]
    pub expected_generation: u64,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=128))]
    pub keep_last: Option<u16>,
    #[arg(long, conflicts_with = "clear_max_total_bytes")]
    pub max_total_bytes: Option<u64>,
    #[arg(long)]
    pub clear_max_total_bytes: bool,
}

#[derive(Args, Debug)]
pub struct ServerBackupPrunePlanArgs {
    pub id: String,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=128))]
    pub keep_last: Option<u16>,
}

#[derive(Args, Debug)]
pub struct ServerBackupPruneArgs {
    pub id: String,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=128))]
    pub keep_last: Option<u16>,
}

#[derive(Subcommand, Debug)]
pub enum ServerRconCommand {
    /// Show the saved RCon settings, whether a protected password is stored, and the owner's live session.
    Status(ServerInstanceSelector),
    /// Turn RCon on in the draft or change its endpoint. Commit, prepare and activate to apply.
    Configure(ServerRconConfigureArgs),
    /// Remove the RCon settings and password reference from the draft. Requires --yes.
    Disable(ServerRconPasswordArgs),
    /// Store a new protected RCon password read from stdin and reference it from the draft.
    SetPassword(ServerRconPasswordArgs),
    /// Round-trip an empty keepalive through the owner's live RCon session.
    Test(ServerInstanceSelector),
    /// Send one command through the owner's live RCon session. Requires --yes; never resent.
    Command(ServerRconCommandArgs),
    /// Show retained RCon command records, newest first.
    History(ServerInstanceSelector),
    /// Open a separate short RCon session to check an endpoint, tunnel or password.
    Probe(ServerRconProbeArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ServerRconPermissionArg {
    Admin,
    Monitor,
}

#[derive(Args, Debug)]
pub struct ServerRconConfigureArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    /// RCon UDP port. For Arma it must stay outside the game port range (game port to game port + 4).
    #[arg(long)]
    pub port: u16,
    /// IPv4 address the server listens on for RCon. Defaults to 127.0.0.1 (this computer only).
    #[arg(long, default_value = "127.0.0.1")]
    pub address: std::net::Ipv4Addr,
    /// Reforger only: what every RCon connection may do.
    #[arg(long, value_enum)]
    pub permission: Option<ServerRconPermissionArg>,
    /// Reforger only: maximum simultaneous RCon clients, 1-16.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
    pub max_clients: Option<u8>,
}

#[derive(Args, Debug)]
pub struct ServerRconPasswordArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerRconCommandArgs {
    pub id: String,
    /// Command text exactly as the server's RCon expects it.
    #[arg(long)]
    pub text: String,
    /// How long to wait for the reply before the outcome is unknown.
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u32).range(1_000..=60_000))]
    pub timeout_ms: u32,
}

#[derive(Args, Debug)]
pub struct ServerRconProbeArgs {
    /// Collect asynchronous server messages for this many milliseconds after the command.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(0..=30_000))]
    pub listen_ms: u32,
    /// Use this instance's active runtime endpoint and protected password.
    pub id: Option<String>,
    /// Connect to this socket address instead, such as a tunnel or port forward.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Read the password from one stdin line instead of the protected store.
    #[arg(long)]
    pub password_stdin: bool,
    /// Send this command once after login (requires --yes). Without it only a keepalive is sent.
    #[arg(long)]
    pub command: Option<String>,
    #[arg(long, default_value_t = 5_000, value_parser = clap::value_parser!(u32).range(500..=60_000))]
    pub timeout_ms: u32,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
pub enum ServerArmaAdminCommand {
    /// List the current player roster.
    Players(ServerArmaAdminRequestArgs),
    /// List current GUID and IP bans.
    Bans(ServerArmaAdminRequestArgs),
    /// List mission templates currently available to the server.
    Missions(ServerArmaAdminRequestArgs),
    /// Send an announcement to all players.
    Announce(ServerArmaAdminMessageArgs),
    /// Kick a player only if the current player id still has the supplied verified GUID.
    Kick(ServerArmaAdminKickArgs),
    /// Add a GUID ban.
    AddBan(ServerArmaAdminAddBanArgs),
    /// Remove a GUID ban only if its current id still has the supplied GUID.
    RemoveBan(ServerArmaAdminRemoveBanArgs),
    /// Lock the server against new players.
    Lock(ServerArmaAdminRequestArgs),
    /// Unlock the server for new players.
    Unlock(ServerArmaAdminRequestArgs),
    /// Change to a mission template currently available to the server.
    Mission(ServerArmaAdminMissionArgs),
    /// Restart the current mission.
    RestartMission(ServerArmaAdminRequestArgs),
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminRequestArgs {
    /// Existing server instance id.
    pub id: String,
    /// Exact active run generation; side-effecting commands require this value.
    #[arg(long)]
    pub expected_run_generation: Option<u64>,
    /// Maximum time to wait for the RCon reply, 1000-30000 ms.
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u32).range(1_000..=30_000))]
    pub timeout_ms: u32,
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminMessageArgs {
    #[command(flatten)]
    pub request: ServerArmaAdminRequestArgs,
    /// Announcement text.
    #[arg(long)]
    pub message: String,
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminKickArgs {
    #[command(flatten)]
    pub request: ServerArmaAdminRequestArgs,
    /// Current player id from a recent players listing.
    #[arg(long)]
    pub player_id: u32,
    /// Verified 32-character GUID from a recent players listing.
    #[arg(long)]
    pub expected_guid: String,
    /// Kick reason.
    #[arg(long)]
    pub reason: String,
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminAddBanArgs {
    #[command(flatten)]
    pub request: ServerArmaAdminRequestArgs,
    /// 32-character BattlEye GUID to ban.
    #[arg(long)]
    pub guid: String,
    /// Ban duration in minutes; zero is passed through to BattlEye.
    #[arg(long)]
    pub minutes: u32,
    /// Ban reason.
    #[arg(long)]
    pub reason: String,
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminRemoveBanArgs {
    #[command(flatten)]
    pub request: ServerArmaAdminRequestArgs,
    /// Current ban id from a recent bans listing.
    #[arg(long)]
    pub ban_id: u32,
    /// 32-character GUID shown for that ban.
    #[arg(long)]
    pub expected_guid: String,
}

#[derive(Args, Debug)]
pub struct ServerArmaAdminMissionArgs {
    #[command(flatten)]
    pub request: ServerArmaAdminRequestArgs,
    /// World-qualified mission basename without the .pbo suffix.
    #[arg(long)]
    pub template: String,
}

#[derive(Args, Debug)]
pub struct ServerHeadlessRestartArgs {
    pub id: String,
    /// Run generation from server instance status.
    #[arg(long)]
    pub expected_run_generation: u64,
    /// Only this headless client id.
    #[arg(long)]
    pub client: Option<String>,
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u32).range(1..=120_000))]
    pub timeout_ms: u32,
}

#[derive(Subcommand, Debug)]
pub enum ServerReforgerAddonsCommand {
    List(ServerInstanceSelector),
    Plan(ServerReforgerAddonPlanArgs),
    Acquire(ServerReforgerAddonAcquireArgs),
}

#[derive(Args, Debug)]
pub struct ServerReforgerAddonPlanArgs {
    pub id: String,
    #[arg(long, value_enum, default_value_t = ServerReforgerAddonChannel::Stable)]
    pub channel: ServerReforgerAddonChannel,
    #[arg(long)]
    pub scenario_provider: Option<String>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ServerReforgerAddonChannel {
    Stable,
    Experimental,
}

#[derive(Args, Debug)]
pub struct ServerReforgerAddonAcquireArgs {
    #[command(flatten)]
    pub inputs: ServerReforgerAddonPlanArgs,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerOwnerArgs {
    #[command(flatten)]
    pub context: ServerContextArgs,
    /// Replay bounded events after this owner-generation sequence.
    #[arg(long)]
    pub events_after: Option<u64>,
    /// Inspect the app-update barrier for all server owners in this config root.
    #[arg(long)]
    pub app_update_check: bool,
}

#[derive(Subcommand, Debug)]
pub enum ServerServiceCommand {
    /// Review or save a new native-host definition; registration remains separate.
    Export {
        #[arg(long, value_parser = ["systemd-user", "windows-user-task"])]
        format: Option<String>,
        #[arg(long)]
        executable: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerPublishCommand {
    /// Review selected player content and the output folder without writing.
    Plan(ServerPublishArgs),
    /// Stage and verify the reviewed output in a new folder. Requires --yes.
    Apply {
        #[command(flatten)]
        inputs: ServerPublishArgs,
        #[arg(long)]
        expected_plan_fingerprint: String,
    },
    /// Check existing output against the selected deployment and options.
    Verify {
        #[command(flatten)]
        inputs: ServerPublishArgs,
        #[arg(long)]
        expected_plan_fingerprint: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct ServerPublishArgs {
    /// Server instance with a selected committed deployment record.
    pub id: String,
    /// Absolute destination folder outside Foxy data, installations and sources.
    #[arg(long)]
    pub output: PathBuf,
    /// Name written to the Arma 3 player repository.
    #[arg(long, default_value = "Player content")]
    pub repository_name: String,
    /// Explicitly include an optional Arma player profile. Repeat for more profiles.
    #[arg(long = "recommend")]
    pub recommendations: Vec<String>,
    /// Include public addon keys in the selected Arma payload.
    #[arg(long)]
    pub include_public_keys: bool,
}

#[derive(Args, Debug)]
pub struct ServerSuperviseArgs {
    #[command(flatten)]
    pub context: ServerContextArgs,
    #[arg(long, hide = true)]
    pub parent_bound: bool,
    /// Run the foreground Windows owner without opening a console window.
    #[arg(long, conflicts_with_all = ["parent_bound", "detach", "shutdown", "launch_id"])]
    pub background: bool,
    /// Keep one authenticated background owner running after this command exits (requires --yes).
    #[arg(long, conflicts_with_all = ["parent_bound", "shutdown", "launch_id"])]
    pub detach: bool,
    /// Shut down the authenticated owner after every session and operation has stopped (requires --yes).
    #[arg(long, conflicts_with_all = ["parent_bound", "detach", "launch_id"])]
    pub shutdown: bool,
    #[arg(long, hide = true, requires = "parent_bound")]
    pub launch_id: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ServerSteamCommand {
    Status(ServerSteamAccountArgs),
    Resume(ServerSteamAccountArgs),
    Anonymous,
    Forget(ServerSteamAccountArgs),
    Login {
        #[command(flatten)]
        account: ServerSteamAccountArgs,
        #[arg(long, conflicts_with = "credentials_stdin")]
        credentials_file: Option<PathBuf>,
        #[arg(long)]
        credentials_stdin: bool,
    },
}

#[derive(Args, Debug)]
pub struct ServerSteamAccountArgs {
    #[arg(
        long,
        default_value = "default",
        help = "Local session alias, not the Steam account name"
    )]
    pub account: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerKeyCommand {
    #[command(alias = "collect")]
    Audit {
        id: String,
        #[arg(long)]
        stage_id: Option<String>,
        #[arg(long = "operator-key")]
        operator_files: Vec<std::path::PathBuf>,
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long = "trust")]
        selected: Vec<String>,
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerMissionCommand {
    List,
    Import {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        apply: bool,
    },
    Refresh {
        mission_id: String,
        #[arg(long)]
        apply: bool,
    },
    Acquire {
        #[arg(long)]
        url: String,
        #[arg(long)]
        template: String,
        #[arg(long)]
        expected_blake3: String,
        #[arg(long)]
        apply: bool,
    },
    Cycle {
        id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerDeployCommand {
    /// Review, activate and recover exact native runtime generations without starting a server.
    Activation {
        #[command(subcommand)]
        command: ServerDeploymentActivationCommand,
    },
    /// Prepare, build and inspect immutable unactivated copies of the selected record.
    Runtime {
        #[command(subcommand)]
        command: ServerDeploymentRuntimeCommand,
    },
    /// Review the selected configuration and an explicitly chosen retained content stage.
    Plan(ServerDeploymentPlanArgs),
    /// Prepare an immutable record of verified archives and an offline saved selection.
    Prepare(ServerDeploymentRecordArgs),
    /// Save the exact reviewed deployment record and its offline selection.
    Commit(ServerDeploymentRecordCommitArgs),
    /// List saved deployment record identifiers without verifying integrity.
    List(ServerInstanceSelector),
    /// Verify a retained record and both referenced archives without current source files.
    Verify(ServerDeploymentRecordVerifyArgs),
    /// Review selecting an indexed retained record without changing current draft or archives.
    SelectPlan(ServerDeploymentRecordSelectionArgs),
    /// Confirm the reviewed offline selection; requires --yes except for dry-run.
    Select(ServerDeploymentRecordSelectArgs),
    /// List deployment records pinned against reconciliation.
    Pins(ServerInstanceSelector),
    /// Pin one indexed deployment record; requires --yes except for dry-run.
    Pin(ServerDeploymentPinArgs),
    /// Remove one pin without deleting anything; requires --yes except for dry-run.
    Unpin(ServerDeploymentPinArgs),
    /// Review and explicitly reclaim unreferenced instance-owned stages, preparations and runtimes.
    Reconcile {
        #[command(subcommand)]
        command: ServerDeploymentReconcileCommand,
    },
}

#[derive(Args, Debug)]
pub struct ServerDeploymentPinArgs {
    pub id: String,
    pub record_id: String,
    #[arg(
        long,
        help = "Require this pin document generation (0 before the first pin)"
    )]
    pub expected_pin_generation: u64,
}

#[derive(Subcommand, Debug)]
pub enum ServerDeploymentReconcileCommand {
    /// Review retained and reclaimable artifacts without writing.
    Plan(ServerInstanceSelector),
    /// Remove exactly the reviewed candidates; requires --yes except for dry-run.
    Apply(ServerDeploymentReconcileApplyArgs),
}

#[derive(Args, Debug)]
pub struct ServerDeploymentReconcileApplyArgs {
    pub id: String,
    #[arg(long, help = "Exact fingerprint returned by deploy reconcile plan")]
    pub expected_plan_fingerprint: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerDeploymentRuntimeCommand {
    /// Review the selected record and exact unactivated generation copy.
    Plan(ServerDeploymentRuntimePlanArgs),
    /// Build the exact reviewed generation without activating runtime files.
    Build(ServerDeploymentRuntimeBuildArgs),
    /// List preparation IDs without claiming integrity.
    List(ServerInstanceSelector),
    /// Verify a prepared generation independently of its original archives.
    Verify(ServerDeploymentRuntimeVerifyArgs),
}

#[derive(Subcommand, Debug)]
pub enum ServerDeploymentActivationCommand {
    Plan(ServerNativeActivationArgs),
    Apply(ServerNativeActivationApplyArgs),
    Status(ServerInstanceSelector),
    RecoveryPlan(ServerInstanceSelector),
    Resolve(ServerNativeResolveArgs),
}

#[derive(Args, Debug)]
pub struct ServerNativeActivationArgs {
    pub id: String,
    pub preparation_id: String,
    #[arg(long)]
    pub expected_deployment_generation: u64,
    #[arg(long)]
    pub expected_activation_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerNativeActivationApplyArgs {
    #[command(flatten)]
    pub inputs: ServerNativeActivationArgs,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerNativeResolveArgs {
    pub id: String,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRuntimePlanArgs {
    pub id: String,
    #[arg(long, help = "Require this selected offline record generation")]
    pub expected_deployment_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRuntimeBuildArgs {
    #[command(flatten)]
    pub inputs: ServerDeploymentRuntimePlanArgs,
    #[arg(
        long,
        help = "Exact review fingerprint returned by deploy runtime plan"
    )]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRuntimeVerifyArgs {
    pub id: String,
    pub preparation_id: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRecordSelectionArgs {
    pub id: String,
    pub record_id: String,
    #[arg(long, help = "Require this saved deployment selection generation")]
    pub expected_deployment_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRecordSelectArgs {
    #[command(flatten)]
    pub inputs: ServerDeploymentRecordSelectionArgs,
    #[arg(long, help = "Exact fingerprint returned by deploy select-plan")]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRecordArgs {
    #[command(flatten)]
    pub inputs: ServerDeploymentPlanArgs,
    #[arg(long, help = "Require this deployment record selection generation")]
    pub expected_deployment_generation: Option<u64>,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRecordCommitArgs {
    #[command(flatten)]
    pub inputs: ServerDeploymentRecordArgs,
    #[arg(long, help = "Exact fingerprint returned by deploy prepare")]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentRecordVerifyArgs {
    pub id: String,
    pub record_id: String,
}

#[derive(Args, Debug)]
pub struct ServerDeploymentPlanArgs {
    pub id: String,
    #[arg(
        long,
        help = "Exact retained stage ID; omission reports missing content without choosing a stage"
    )]
    pub stage: Option<String>,
    #[arg(long, help = "Require this saved instance revision")]
    pub expected_revision: Option<u64>,
    #[arg(long, help = "Require this configuration selection generation")]
    pub expected_configuration_generation: Option<u64>,
}

#[derive(Subcommand, Debug)]
pub enum ServerContentCommand {
    /// Review or acquire repository content into an inactive retained server stage.
    Acquire {
        #[command(subcommand)]
        command: ServerContentAcquisitionCommand,
    },
    /// List addon folders and repository profiles.
    List,
    /// Record an absolute folder that holds addon directories.
    AddFolder(ServerContentFolderArgs),
    /// Bind one repository to a folder as a download or an already-present copy.
    AddProfile(ServerContentProfileArgs),
    /// Preview ordered source bindings for a saved server instance without downloading or copying.
    Plan(ServerContentPlanArgs),
    /// Inspect, review and save instance-owned content selection without acquiring files.
    Bindings {
        #[command(subcommand)]
        command: ServerContentBindingsCommand,
    },
    /// Verify local bytes and retain unselected content stages without activation.
    Stage {
        #[command(subcommand)]
        command: ServerContentStageCommand,
    },
    /// Report which server instances, client repositories and other spaces use each source.
    Consumers,
}

#[derive(Subcommand, Debug)]
pub enum ServerContentAcquisitionCommand {
    Plan(ServerContentAcquisitionPlanArgs),
    Apply(ServerContentAcquisitionApplyArgs),
}

#[derive(Args, Debug)]
pub struct ServerContentAcquisitionPlanArgs {
    pub id: String,
    #[arg(
        long,
        help = "Exact verified retained stage to copy as a private patch seed"
    )]
    pub seed_stage: Option<String>,
    #[arg(long)]
    pub expected_bindings_fingerprint: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServerContentAcquisitionApplyArgs {
    #[command(flatten)]
    pub inputs: ServerContentAcquisitionPlanArgs,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerContentStageCommand {
    /// Hash current saved already-present content without writing files.
    Plan(ServerContentStagePlanArgs),
    /// Copy exactly the reviewed local content into a new immutable stage.
    Apply(ServerContentStageApplyArgs),
    /// Verify the exact manifest and file bytes of a retained stage.
    Verify(ServerContentStageVerifyArgs),
    /// List stage identifiers without claiming integrity.
    List(ServerContentBindingsShowArgs),
}

#[derive(Args, Debug)]
pub struct ServerContentStagePlanArgs {
    pub id: String,
    #[arg(
        long,
        help = "Require this saved content binding fingerprint before verification"
    )]
    pub expected_bindings_fingerprint: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServerContentStageApplyArgs {
    #[command(flatten)]
    pub inputs: ServerContentStagePlanArgs,
    #[arg(
        long,
        help = "Fingerprint from content stage plan for these exact bytes and bindings"
    )]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerContentStageVerifyArgs {
    pub id: String,
    pub stage_id: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerContentBindingsCommand {
    /// Inspect saved selections and whether their reviewed inputs are still current.
    Show(ServerContentBindingsShowArgs),
    /// Review a replacement selection and obtain its save fingerprint without writes.
    Plan(ServerContentPlanArgs),
    /// Save exactly the reviewed selection under the exclusive game-space claim.
    Save(ServerContentBindingsSaveArgs),
}

#[derive(Args, Debug)]
pub struct ServerContentBindingsShowArgs {
    pub id: String,
}

#[derive(Args, Debug)]
pub struct ServerContentBindingsSaveArgs {
    #[command(flatten)]
    pub inputs: ServerContentPlanArgs,
    #[arg(
        long,
        help = "Fingerprint returned by content bindings plan for these exact inputs"
    )]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerContentPlanArgs {
    pub id: String,
    #[arg(
        long,
        help = "JSON array of profile source_id, role and order bindings"
    )]
    pub bindings: std::path::PathBuf,
    #[arg(
        long,
        help = "Require this saved instance revision before generating the preview"
    )]
    pub expected_revision: Option<u64>,
    #[arg(
        long,
        help = "Require this source-registry fingerprint from content list"
    )]
    pub expected_sources_fingerprint: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServerContentFolderArgs {
    #[arg(long, help = "Short folder id, such as hosted or incoming")]
    pub id: String,
    #[arg(long, help = "Absolute folder that contains addon directories")]
    pub path: std::path::PathBuf,
}

#[derive(Args, Debug)]
pub struct ServerContentProfileArgs {
    #[arg(long, help = "Profile name")]
    pub name: String,
    #[arg(long, help = "Folder id from add-folder")]
    pub folder: String,
    #[arg(
        long,
        default_value = "",
        help = "Repository URL; omit only for already-present local content"
    )]
    pub url: String,
    #[arg(
        long,
        help = "Repository local path. Same path as the folder means the server already hosts it"
    )]
    pub local_path: std::path::PathBuf,
    #[arg(long, value_parser = ["download", "already-present"], help = "download acquires the repository later; already-present does not")]
    pub supply: String,
    #[arg(
        long,
        help = "Addon directory name inside the folder. Repeat for more than one. Omit to use the whole folder"
    )]
    pub addon: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ServerContextArgs {
    #[arg(
        long,
        conflicts_with = "wipe_db",
        global = true,
        help = "Select an existing game space without changing the remembered selection"
    )]
    pub game_space: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServerInspectAddonsArgs {
    #[arg(long, help = "Server address or host name")]
    pub address: String,
    #[arg(long, help = "Arma 3 game port; Foxy queries port + 1")]
    pub port: u16,
    #[arg(long, help = "Include all returned Steam server rules in the output")]
    pub include_rules: bool,
}

#[derive(Subcommand, Debug)]
pub enum ServerInstanceCommand {
    /// Render saved typed configuration without writing files or starting a server.
    Render(ServerInstanceSelector),
    /// Inspect the saved instance's proposed layout and outstanding deployment blockers.
    Preflight(ServerInstanceSelector),
    /// Adopt an existing installation and bind it to this draft; no game files are changed.
    Adopt(ServerInstanceAdoptArgs),
    /// Bind or unbind a previously adopted installation in this game space.
    Bind(ServerInstanceBindArgs),
    /// List saved drafts in catalog order.
    List,
    /// Show a typed draft. In structured output, data.instance can be edited for replace.
    Show(ServerInstanceSelector),
    /// Create a draft with explicit safe initial policy for the selected game.
    Add(ServerInstanceNameArgs),
    /// Copy a draft under a new stable id and name; no runtime data is copied.
    Clone(ServerInstanceEditArgs),
    /// Change only the display name, preserving game configuration.
    Rename(ServerInstanceEditArgs),
    /// Replace a draft from its typed JSON document, requiring a matching revision.
    Replace(ServerInstanceFileArgs),
    /// Validate a typed draft JSON document without writing or claiming a database.
    Validate(ServerInstanceValidateArgs),
    /// Remove only a draft from the catalog; requires --yes and a matching revision.
    Remove(ServerInstanceRemoveArgs),
    /// Print the dedicated launch arguments without starting a process.
    LaunchPlan(ServerInstanceSelector),
    /// Review the exact committed native runtime without starting an owner or process.
    StartPlan(ServerInstanceSelector),
    /// Review exact consumer records and native crash cleanup receipts without writing.
    RecoveryPlan(ServerInstanceSelector),
    /// Retire only exit-proven old-owner records from an exact reviewed recovery plan.
    Recover(ServerInstanceRecoverArgs),
    /// Start a reviewed runtime through an existing foreground owner.
    Start(ServerInstanceStartArgs),
    /// Inspect the retained session through its authenticated foreground owner.
    Status(ServerInstanceSelector),
    /// Read cached process, query, resource, and bounded log observations.
    Monitor(ServerInstanceMonitorArgs),
    /// Read and filter the bounded log snapshot cached for the current run.
    Logs(ServerInstanceLogsArgs),
    /// Request native shutdown and wait; force escalation requires --force and --yes.
    Stop(ServerInstanceStopArgs),
    /// Stop the exact owned run, then start the reviewed committed runtime.
    Restart(ServerInstanceRestartArgs),
}

#[derive(Args, Debug)]
pub struct ServerInstanceRecoverArgs {
    pub id: String,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerInstanceMonitorArgs {
    pub id: String,
    /// Follow monitoring snapshots until Ctrl+C. Requires --stream-json.
    #[arg(long)]
    pub follow: bool,
    /// Poll interval for --follow, from 250 through 30000 milliseconds.
    #[arg(long, value_parser = clap::value_parser!(u32).range(250..=30_000))]
    pub interval_ms: Option<u32>,
    /// Emit framed newline-delimited JSON. Only valid with --follow.
    #[arg(long)]
    pub stream_json: bool,
}

#[derive(Args, Debug)]
pub struct ServerInstanceLogsArgs {
    pub id: String,
    /// Keep lines containing this case-insensitive text.
    #[arg(long)]
    pub contains: Option<String>,
    /// Keep lines with a sequence greater than this value.
    #[arg(long)]
    pub after_sequence: Option<u64>,
    /// Maximum returned lines, from 1 through 128.
    #[arg(long, default_value_t = 128)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct ServerInstanceStartArgs {
    pub id: String,
    #[arg(long)]
    pub expected_activation_generation: Option<u64>,
    #[arg(long)]
    pub expected_plan_fingerprint: Option<String>,
    /// Establish or attach to a durable background owner before starting (requires --yes).
    #[arg(long)]
    pub detach: bool,
}

#[derive(Args, Debug)]
pub struct ServerInstanceStopArgs {
    pub id: String,
    #[arg(long)]
    pub expected_run_generation: u64,
    #[arg(long)]
    pub force: bool,
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u32).range(1..=120_000))]
    pub timeout_ms: u32,
}

#[derive(Args, Debug)]
pub struct ServerInstanceRestartArgs {
    pub id: String,
    #[arg(long)]
    pub expected_run_generation: u64,
    #[arg(long)]
    pub expected_activation_generation: u64,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
    #[arg(long)]
    pub force: bool,
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u32).range(1..=120_000))]
    pub timeout_ms: u32,
}

#[derive(Args, Debug)]
pub struct ServerInstanceSelector {
    pub id: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerInstallationCommand {
    /// List recorded installations; recorded evidence may be stale.
    List,
    /// Inspect the expected native dedicated executable without executing it.
    Inspect(ServerInstallationInspectArgs),
    /// Inspect registered consumers without acquiring installation leases.
    Consumers(ServerInstallationConsumersArgs),
    /// Review or install a managed dedicated server from native Steam depots.
    Install {
        #[command(subcommand)]
        command: ServerNativeInstallCommand,
    },
    /// Compare the managed installation with its Steam branch without downloading.
    Check(ServerNativeCheckArgs),
    /// Re-hash the complete installed inventory offline; writes nothing.
    Validate(ServerInstanceSelector),
    /// Review and publish a successor build, retaining the previous installation.
    Update {
        #[command(subcommand)]
        command: ServerNativeInstallCommand,
    },
    /// Reacquire a reviewed complete build into a new folder and switch this instance.
    Repair {
        #[command(subcommand)]
        command: ServerNativeInstallCommand,
    },
}

#[derive(Args, Debug)]
pub struct ServerNativeCheckArgs {
    pub id: String,
    #[arg(long, default_value = "default", conflicts_with = "anonymous")]
    pub account: String,
    #[arg(long)]
    pub anonymous: bool,
}

#[derive(Subcommand, Debug)]
pub enum ServerWorkshopCommand {
    /// List tracked Workshop items and their pinned revisions.
    List,
    /// Read-only Steam review of items to acquire, update or repair; writes nothing.
    Plan(ServerWorkshopArgs),
    /// Download, verify and pin the reviewed items as already-present content profiles.
    Acquire(ServerWorkshopAcquireArgs),
    /// Re-hash the pinned revision of a tracked item without writing.
    Verify(ServerWorkshopItemArgs),
}

#[derive(Args, Debug)]
pub struct ServerWorkshopArgs {
    /// Workshop item ids or URLs.
    #[arg(required = true)]
    pub items: Vec<String>,
    /// Prepared native Steam session alias from `server steam login`.
    #[arg(long, default_value = "default", conflicts_with = "anonymous")]
    pub account: String,
    /// Use an anonymous Steam connection; Arma Workshop downloads still need ownership.
    #[arg(long)]
    pub anonymous: bool,
    /// Download the pinned revision of tracked items again and replace a damaged copy.
    #[arg(long)]
    pub repair: bool,
}

#[derive(Args, Debug)]
pub struct ServerWorkshopAcquireArgs {
    #[command(flatten)]
    pub inputs: ServerWorkshopArgs,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerWorkshopItemArgs {
    pub item: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerNativeInstallCommand {
    /// Read-only Steam review of the exact build, depots and disk need; writes nothing.
    Plan(ServerNativeInstallArgs),
    /// Download, verify and publish the reviewed build into a new folder, then bind it.
    Apply(ServerNativeInstallApplyArgs),
}

#[derive(Args, Debug)]
pub struct ServerNativeInstallArgs {
    /// Instance that receives the managed installation.
    pub id: String,
    /// New installation folder outside the Foxy data directory; it must not exist yet.
    #[arg(long)]
    pub directory: PathBuf,
    #[arg(long, default_value = "public")]
    pub branch: String,
    /// Dedicated server content channel; omitted maintenance requests keep the installed channel.
    #[arg(long, value_enum)]
    pub channel: Option<ServerNativeInstallChannel>,
    /// Prepared native Steam session alias from `server steam login`.
    #[arg(long, default_value = "default", conflicts_with = "anonymous")]
    pub account: String,
    /// Use an anonymous Steam connection instead of a prepared session.
    #[arg(long)]
    pub anonymous: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ServerNativeInstallChannel {
    Stable,
    Experimental,
}

#[derive(Args, Debug)]
pub struct ServerNativeInstallApplyArgs {
    #[command(flatten)]
    pub install: ServerNativeInstallArgs,
    #[arg(long)]
    pub expected_plan_fingerprint: String,
}

#[derive(Subcommand, Debug)]
pub enum ServerConfigurationCommand {
    /// Review a vendor configuration import; --apply --yes saves the reviewed draft.
    Import(ServerConfigurationImportArgs),
    /// Export a redacted portable instance pack to a new file.
    ExportPack(ServerConfigurationPackExportArgs),
    /// Review a portable pack as a new unbound instance; --apply --yes saves it.
    ImportPack(ServerConfigurationPackImportArgs),
    /// Review a Reforger server.json file. --apply --yes replaces the saved draft.
    ImportReforger(ServerReforgerImportArgs),
    /// Write the saved Reforger draft as redacted server.json without overwriting a file.
    ExportReforger(ServerReforgerExportArgs),
    /// Parse or store a Reforger engine scenario listing without changing the selected scenario.
    #[command(subcommand)]
    Scenario(ServerScenarioCommand),
    Plan(ServerInstanceSelector),
    List(ServerInstanceSelector),
    Commit(ServerConfigurationCommitArgs),
    Verify(ServerConfigurationRevisionArgs),
    Rollback(ServerConfigurationRollbackArgs),
}

#[derive(Args, Debug)]
pub struct ServerConfigurationImportArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[arg(long)]
    pub server: Option<PathBuf>,
    #[arg(long)]
    pub basic: Option<PathBuf>,
    #[arg(long)]
    pub profile: Option<PathBuf>,
    #[arg(
        long,
        help = "Explicitly convert supported fields to managed output; unknown directives are omitted after review"
    )]
    pub managed: bool,
    #[arg(long)]
    pub apply: bool,
    #[arg(
        long,
        help = "Acknowledge unresolved operator paths without opening or copying them"
    )]
    pub review_paths: bool,
}

#[derive(Args, Debug)]
pub struct ServerConfigurationPackExportArgs {
    pub id: String,
    #[arg(long)]
    pub file: PathBuf,
    #[arg(
        long,
        help = "Remove operator text and policy identifiers; record every omission in an incomplete pack"
    )]
    pub sanitized: bool,
}

#[derive(Args, Debug)]
pub struct ServerConfigurationPackImportArgs {
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub review_paths: bool,
}

#[derive(Args, Debug)]
pub struct ServerReforgerImportArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub apply: bool,
}

#[derive(Args, Debug)]
pub struct ServerReforgerExportArgs {
    pub id: String,
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum ServerScenarioCommand {
    /// Parse an engine log and report the official scenario listing. No files are written.
    ParseLog {
        #[arg(long)]
        file: PathBuf,
    },
    /// Store a complete listing beside the instance. A failed log does not replace a stored listing or the selected scenario.
    ImportLog {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Show the stored listing and whether it contains the selected scenario.
    List { id: String },
}

#[derive(Args, Debug)]
pub struct ServerConfigurationCommitArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[arg(long)]
    pub expected_generation: u64,
    #[arg(long)]
    pub expected_fingerprint: String,
}

#[derive(Args, Debug)]
pub struct ServerConfigurationRevisionArgs {
    pub id: String,
    #[arg(long)]
    pub revision: String,
}

#[derive(Args, Debug)]
pub struct ServerConfigurationRollbackArgs {
    pub id: String,
    #[arg(long)]
    pub revision: String,
    #[arg(long)]
    pub expected_generation: u64,
}

#[derive(Args, Debug)]
pub struct ServerInstallationInspectArgs {
    #[arg(long)]
    pub directory: PathBuf,
}

#[derive(Args, Debug)]
pub struct ServerInstallationConsumersArgs {
    pub id: String,
}

#[derive(Args, Debug)]
pub struct ServerInstanceAdoptArgs {
    pub id: String,
    #[arg(long)]
    pub directory: PathBuf,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerInstanceBindArgs {
    pub id: String,
    #[arg(long, required_unless_present = "clear", conflicts_with = "clear")]
    pub installation: Option<String>,
    #[arg(long)]
    pub clear: bool,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerInstanceNameArgs {
    #[arg(long)]
    pub name: String,
}

#[derive(Args, Debug)]
pub struct ServerInstanceEditArgs {
    pub id: String,
    #[arg(long)]
    pub name: String,
    #[arg(long, help = "Current draft revision, from instance show")]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerInstanceFileArgs {
    #[arg(long, help = "Typed Foxy instance JSON, not a vendor server config")]
    pub file: PathBuf,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct ServerInstanceValidateArgs {
    #[arg(long, help = "Typed Foxy instance JSON, not a vendor server config")]
    pub file: PathBuf,
}

#[derive(Args, Debug)]
pub struct ServerInstanceRemoveArgs {
    pub id: String,
    #[arg(long)]
    pub expected_revision: u64,
}

#[derive(Args, Debug)]
pub struct DirectDownloadArgs {
    #[arg(long, help = "Source URL (repository, addon, or file)")]
    pub address: String,
    #[arg(
        long,
        help = "Destination folder (defaults to temp directory or app data dir)"
    )]
    pub dest: Option<PathBuf>,
    #[arg(
        long,
        help = "Explicitly inherit global speed limit from settings (default behavior)"
    )]
    pub use_global_speed_limit: bool,
    #[arg(long, help = "Disable speed limit for this command")]
    pub unlimited: bool,
    #[arg(long, help = "Set speed limit in Mbps for this command")]
    pub limit_mbps: Option<u32>,
    #[arg(
        long,
        help = "Approve replacing existing targets (also requires --yes)"
    )]
    pub overwrite: bool,
}

#[derive(Args, Debug)]
pub struct LaunchArgs {
    #[arg(long, help = "Keep the changed destination for this extra-file id")]
    pub keep_extra_file: Vec<String>,
    #[arg(long, help = "Reapply the stored template for this extra-file id")]
    pub reapply_extra_file: Vec<String>,
    #[arg(long, help = "Approve the displayed ordered extra-file replacements")]
    pub approve_extra_file_layers: bool,
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Optional server name from repository server list")]
    pub server: Option<String>,
    #[arg(
        long,
        help = "Execute launch immediately (without this flag, command prints launch spec)"
    )]
    pub execute: bool,
    #[arg(
        long,
        help = "Allow this launch to continue if extra files cannot be applied"
    )]
    pub allow_incomplete_preparation: bool,
    #[arg(
        long,
        help = "Allow this launch to use live Workshop files when a frozen copy is missing"
    )]
    pub allow_live_fallback: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_monitor_parser_keeps_snapshot_and_stream_modes_explicit() {
        let parsed = CliArgs::try_parse_from([
            "foxy",
            "server",
            "instance",
            "--game-space",
            "alpha",
            "monitor",
            "server_a",
        ])
        .unwrap();
        let Some(CliCommand::Server {
            command:
                ServerCommand::Instance {
                    command: ServerInstanceCommand::Monitor(args),
                    ..
                },
        }) = parsed.command
        else {
            panic!("unexpected command");
        };
        assert!(!args.follow);
        assert!(!args.stream_json);
        assert_eq!(args.interval_ms, None);

        let parsed = CliArgs::try_parse_from([
            "foxy",
            "server",
            "instance",
            "monitor",
            "server_a",
            "--follow",
            "--stream-json",
            "--interval-ms",
            "250",
        ])
        .unwrap();
        let Some(CliCommand::Server {
            command:
                ServerCommand::Instance {
                    command: ServerInstanceCommand::Monitor(args),
                    ..
                },
        }) = parsed.command
        else {
            panic!("unexpected command");
        };
        assert!(args.follow && args.stream_json);
        assert_eq!(args.interval_ms, Some(250));
        assert!(
            CliArgs::try_parse_from([
                "foxy",
                "server",
                "instance",
                "monitor",
                "server_a",
                "--follow",
                "--stream-json",
                "--interval-ms",
                "249",
            ])
            .is_err()
        );
    }

    #[test]
    fn extra_file_order_can_change_without_changing_enablement() {
        let parsed = CliArgs::try_parse_from([
            "foxy",
            "config",
            "extra-file",
            "set",
            "alpha",
            "--order",
            "0",
        ])
        .unwrap();
        let Some(CliCommand::Config {
            command:
                ConfigCommand::ExtraFile {
                    command: ConfigExtraFileCommand::Set(args),
                },
        }) = parsed.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(args.enabled, None);
        assert_eq!(args.order, Some(0));
        let parsed = CliArgs::try_parse_from([
            "foxy",
            "config",
            "extra-file",
            "set",
            "alpha",
            "--enabled",
            "false",
        ])
        .unwrap();
        let Some(CliCommand::Config {
            command:
                ConfigCommand::ExtraFile {
                    command: ConfigExtraFileCommand::Set(args),
                },
        }) = parsed.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(args.enabled, Some(false));
    }

    #[test]
    fn workshop_selection_accepts_explicit_boolean_and_legacy_flag_forms() {
        for (arguments, expected) in [
            (
                vec!["foxy", "workshop", "set", "123456789", "--enabled", "false"],
                false,
            ),
            (
                vec!["foxy", "workshop", "set", "123456789", "--enabled", "true"],
                true,
            ),
            (
                vec!["foxy", "workshop", "set", "123456789", "--enabled"],
                true,
            ),
            (vec!["foxy", "workshop", "set", "123456789"], false),
        ] {
            let parsed = CliArgs::try_parse_from(arguments).unwrap();
            let Some(CliCommand::Workshop {
                command: WorkshopCommand::Set(args),
            }) = parsed.command
            else {
                panic!("unexpected command");
            };
            assert_eq!(args.enabled, expected);
        }
        assert!(
            CliArgs::try_parse_from([
                "foxy",
                "workshop",
                "set",
                "123456789",
                "--enabled",
                "invalid"
            ])
            .is_err()
        );
    }
}
