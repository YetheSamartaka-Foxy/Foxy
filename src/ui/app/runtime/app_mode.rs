use crate::core::app_mode::AppMode;
use crate::ui::app::Foxy;
use crate::ui::types::FoxyView;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AppModeNavigation {
    observed_mode: AppMode,
    client_views: (FoxyView, FoxyView),
    server_views: (FoxyView, FoxyView),
}

impl Default for AppModeNavigation {
    fn default() -> Self {
        Self {
            observed_mode: AppMode::Client,
            client_views: (FoxyView::RepositoryList, FoxyView::None),
            server_views: (FoxyView::ServerInstances, FoxyView::None),
        }
    }
}

impl AppModeNavigation {
    fn sync(
        &mut self,
        mode: AppMode,
        current_view: FoxyView,
        last_view: FoxyView,
    ) -> (FoxyView, FoxyView) {
        let outgoing = sanitize_views(self.observed_mode, (current_view, last_view));
        match self.observed_mode {
            AppMode::Client => self.client_views = outgoing,
            AppMode::Server => self.server_views = outgoing,
        }
        self.observed_mode = mode;
        let active = match mode {
            AppMode::Client => &mut self.client_views,
            AppMode::Server => &mut self.server_views,
        };
        *active = sanitize_views(mode, *active);
        *active
    }
}

fn home_view(mode: AppMode) -> FoxyView {
    match mode {
        AppMode::Client => FoxyView::RepositoryList,
        AppMode::Server => FoxyView::ServerInstances,
    }
}

fn safe_view(mode: AppMode, view: FoxyView) -> FoxyView {
    match (mode, view) {
        (_, FoxyView::None)
        | (AppMode::Client, FoxyView::ServerInstances)
        | (
            AppMode::Server,
            FoxyView::RepositoryList
            | FoxyView::RepositorySettings
            | FoxyView::RepositorySpaceSettings
            | FoxyView::SwiftyMigration,
        ) => home_view(mode),
        _ => view,
    }
}

fn sanitize_views(mode: AppMode, views: (FoxyView, FoxyView)) -> (FoxyView, FoxyView) {
    (
        safe_view(mode, views.0),
        if views.1 == FoxyView::None {
            FoxyView::None
        } else {
            safe_view(mode, views.1)
        },
    )
}

impl Foxy {
    pub(crate) fn effective_app_mode(&self) -> AppMode {
        self.mode_ownership.active
    }

    pub(crate) fn sync_app_mode_navigation(&mut self) {
        let mode = self.effective_app_mode();
        (self.current_view, self.last_view) =
            self.mode_navigation
                .sync(mode, self.current_view, self.last_view);
    }

    pub(crate) fn mode_home_view(&self) -> FoxyView {
        home_view(self.effective_app_mode())
    }

    pub(crate) fn mode_safe_view(&self, view: FoxyView) -> FoxyView {
        safe_view(self.effective_app_mode(), view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_defaults_to_independent_mode_homes() {
        let mut navigation = AppModeNavigation::default();
        assert_eq!(navigation.observed_mode, AppMode::Client);
        assert_eq!(
            navigation.sync(AppMode::Server, FoxyView::RepositoryList, FoxyView::None),
            (FoxyView::ServerInstances, FoxyView::None)
        );
        assert_eq!(
            navigation.sync(AppMode::Client, FoxyView::ServerInstances, FoxyView::None),
            (FoxyView::RepositoryList, FoxyView::None)
        );
    }

    #[test]
    fn navigation_round_trip_preserves_each_modes_current_and_previous_views() {
        let mut navigation = AppModeNavigation::default();
        assert_eq!(
            navigation.sync(
                AppMode::Server,
                FoxyView::Settings,
                FoxyView::RepositorySettings,
            ),
            (FoxyView::ServerInstances, FoxyView::None)
        );
        assert_eq!(
            navigation.sync(AppMode::Client, FoxyView::Help, FoxyView::ServerInstances),
            (FoxyView::Settings, FoxyView::RepositorySettings)
        );
        assert_eq!(
            navigation.sync(
                AppMode::Server,
                FoxyView::GameSpaces,
                FoxyView::RepositoryList
            ),
            (FoxyView::Help, FoxyView::ServerInstances)
        );
        assert_eq!(
            navigation.sync(AppMode::Client, FoxyView::Help, FoxyView::ServerInstances),
            (FoxyView::GameSpaces, FoxyView::RepositoryList)
        );
    }

    #[test]
    fn mode_sanitization_rejects_other_modes_views_and_uses_its_home_for_none() {
        for view in [
            FoxyView::RepositoryList,
            FoxyView::RepositorySettings,
            FoxyView::RepositorySpaceSettings,
            FoxyView::SwiftyMigration,
            FoxyView::None,
        ] {
            assert_eq!(safe_view(AppMode::Server, view), FoxyView::ServerInstances);
        }
        for view in [FoxyView::ServerInstances, FoxyView::None] {
            assert_eq!(safe_view(AppMode::Client, view), FoxyView::RepositoryList);
        }
        for mode in [AppMode::Client, AppMode::Server] {
            for view in [
                FoxyView::Settings,
                FoxyView::Help,
                FoxyView::Changelog,
                FoxyView::About,
                FoxyView::AppUpdate,
                FoxyView::VersionBrowser,
                FoxyView::GameSpaces,
                FoxyView::GameSpaceSettings,
            ] {
                assert_eq!(safe_view(mode, view), view);
            }
            assert_eq!(
                sanitize_views(mode, (FoxyView::None, FoxyView::None)),
                (home_view(mode), FoxyView::None)
            );
        }
    }

    #[test]
    fn navigation_sanitizes_direct_view_changes_without_switching_modes() {
        let mut navigation = AppModeNavigation::default();
        assert_eq!(
            navigation.sync(AppMode::Client, FoxyView::ServerInstances, FoxyView::Help),
            (FoxyView::RepositoryList, FoxyView::Help)
        );
        navigation.sync(AppMode::Server, FoxyView::RepositoryList, FoxyView::None);
        assert_eq!(
            navigation.sync(
                AppMode::Server,
                FoxyView::RepositorySettings,
                FoxyView::RepositorySpaceSettings,
            ),
            (FoxyView::ServerInstances, FoxyView::ServerInstances)
        );
    }
}
