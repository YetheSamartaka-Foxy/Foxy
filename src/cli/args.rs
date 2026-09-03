use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "foxy")]
#[command(about = "Foxy command line interface")]
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
    #[arg(long, help = "Show or hide debug windows (true/false)")]
    pub show_debug_windows: Option<bool>,
    #[arg(long, help = "Show or hide footer activity log toggle (true/false)")]
    pub show_activity_log: Option<bool>,
    #[arg(
        long,
        help = "Show or hide memory diagnostics icon in footer (true/false)"
    )]
    pub show_memory_diagnostics_icon: Option<bool>,
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
        help = "Automatically run quick local scan on startup (true/false)"
    )]
    pub auto_quick_scan_on_launch: Option<bool>,
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
}

#[derive(Args, Debug)]
pub struct GameLaunchArgs {
    #[arg(
        long,
        help = "Execute launch immediately (without this flag, command prints launch spec)"
    )]
    pub execute: bool,
    #[arg(long, help = "Include disabled Workshop items in the launch manifest")]
    pub include_disabled: bool,
    #[arg(long, help = "Optional TW:WH3 save name for continue-campaign launch")]
    pub save_name: Option<String>,
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
    Activate,
}

#[derive(Args, Debug)]
pub struct ConfigExtraFileAddArgs {
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
    /// Extra-file id from `foxy config extra-file list`.
    pub id: String,
    #[arg(long, help = "Target enabled state (true/false)")]
    pub enabled: bool,
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
    #[arg(long, help = "Delete Steam's content folder and Foxy's frozen copies")]
    pub delete_data: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = WorkshopDownloadBackend::SteamHelper,
        help = "Unsubscribe backend: steam-helper or none"
    )]
    pub backend: WorkshopDownloadBackend,
    #[arg(long, default_value_t = 300, help = "Steam helper timeout in seconds")]
    pub timeout_seconds: u64,
}

#[derive(Args, Debug)]
pub struct WorkshopSetArgs {
    /// Steam Workshop item id or URL.
    pub item: String,
    #[arg(long, help = "Target enabled state (true/false)")]
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
    /// Query public server addon metadata through Steam server rules.
    InspectAddons(ServerInspectAddonsArgs),
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
}

#[derive(Args, Debug)]
pub struct LaunchArgs {
    #[command(flatten)]
    pub selector: RepoSelectorArgs,
    #[arg(long, help = "Optional server name from repository server list")]
    pub server: Option<String>,
    #[arg(
        long,
        help = "Execute launch immediately (without this flag, command prints launch spec)"
    )]
    pub execute: bool,
}
