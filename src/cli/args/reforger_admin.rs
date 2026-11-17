use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct ServerReforgerAdminRequestArgs {
    pub id: String,
    #[arg(long)]
    pub expected_run_generation: Option<u64>,
    /// Maximum command or scenario-reload wait, 1000-60000 ms.
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u32).range(1_000..=60_000))]
    pub timeout_ms: u32,
}

#[derive(Subcommand, Debug)]
pub enum ServerReforgerAdminCommand {
    /// List the current player roster.
    Players(ServerReforgerAdminRequestArgs),
    /// List one page of current bans.
    Bans(ServerReforgerAdminBansArgs),
    /// Kick a player only if the current player id still has the supplied identity.
    Kick(ServerReforgerAdminKickArgs),
    /// Ban a persistent player identity.
    Ban(ServerReforgerAdminBanArgs),
    /// Remove a ban by persistent player identity.
    RemoveBan(ServerReforgerAdminIdentityArgs),
    /// Reload the current scenario.
    ReloadScenario(ServerReforgerAdminRequestArgs),
    /// De-authenticate this RCon client.
    Logout(ServerReforgerAdminRequestArgs),
}

#[derive(Args, Debug)]
pub struct ServerReforgerAdminBansArgs {
    #[command(flatten)]
    pub request: ServerReforgerAdminRequestArgs,
    /// One-based ban list page.
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub page: u32,
}

#[derive(Args, Debug)]
pub struct ServerReforgerAdminKickArgs {
    #[command(flatten)]
    pub request: ServerReforgerAdminRequestArgs,
    /// Current player id from a recent player listing.
    #[arg(long)]
    pub player_id: u32,
    /// Exact persistent identity shown for that player.
    #[arg(long)]
    pub expected_identity: String,
}

#[derive(Args, Debug)]
pub struct ServerReforgerAdminBanArgs {
    #[command(flatten)]
    pub request: ServerReforgerAdminRequestArgs,
    /// Persistent player identity, not a transient player id or name.
    #[arg(long)]
    pub identity: String,
    /// Ban duration in seconds; zero is permanent.
    #[arg(long)]
    pub duration_seconds: u32,
    /// Single-line ban reason.
    #[arg(long)]
    pub reason: String,
}

#[derive(Args, Debug)]
pub struct ServerReforgerAdminIdentityArgs {
    #[command(flatten)]
    pub request: ServerReforgerAdminRequestArgs,
    /// Persistent player identity to unban.
    #[arg(long)]
    pub identity: String,
}
