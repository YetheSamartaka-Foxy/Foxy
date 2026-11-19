#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum ServerPage {
    #[default]
    Overview,
    Add,
    Setup,
    Installation,
    Import,
    Configuration,
    Addons,
    Profiles,
    Deployment,
    Content,
    Records,
    Operations,
    Administration,
    Maintenance,
    Recovery,
    Tools,
}

impl ServerPage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Add => "Add server",
            Self::Setup => "Setup",
            Self::Installation => "Server installation",
            Self::Import => "Import",
            Self::Configuration => "Saved configuration revisions",
            Self::Addons => "Addons",
            Self::Profiles => "Addon profiles",
            Self::Deployment => "Deployment",
            Self::Content => "Server content",
            Self::Records => "Deployment records",
            Self::Operations => "Operations",
            Self::Administration => "Administration",
            Self::Maintenance => "Scheduled maintenance",
            Self::Recovery => "Backups and recovery",
            Self::Tools => "Tools",
        }
    }

    pub fn group(self) -> Self {
        match self {
            Self::Installation | Self::Import | Self::Configuration => Self::Setup,
            Self::Profiles => Self::Addons,
            Self::Content | Self::Records => Self::Deployment,
            Self::Administration | Self::Maintenance => Self::Operations,
            page => page,
        }
    }

    pub fn subpages(self) -> &'static [Self] {
        match self.group() {
            Self::Setup => &[
                Self::Setup,
                Self::Installation,
                Self::Import,
                Self::Configuration,
            ],
            Self::Deployment => &[Self::Content, Self::Records, Self::Deployment],
            Self::Addons => &[Self::Addons, Self::Profiles],
            Self::Operations => &[Self::Operations, Self::Administration, Self::Maintenance],
            _ => &[],
        }
    }
}
use crate::core::server::{ContentSources, RepositoryProfile};

#[derive(Default)]
pub(crate) struct ServerProfileEditor {
    pub initialized: bool,
    pub selected_profile: String,
    pub folder_edit: Option<(String, String)>,
    pub profile: Option<RepositoryProfile>,
    pub base_fingerprint: Option<String>,
    pub existing: bool,
    pub filter: String,
    pub addon_name: String,
    pub review: Option<ContentSources>,
    pub error: Option<String>,
}
