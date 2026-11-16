use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerPlatform, ServerStore, ServerStoreError,
};

const MAX_PROFILE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct NativeProfileSeed {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub profile_name: String,
    pub blake3: String,
}

/// Directory the dedicated engine creates under `-profiles` for a `-name` profile: observed
/// as `Users` on Windows and `home` on Linux in accepted 2.22 runs.
pub(super) fn profile_user_directory(platform: ServerPlatform) -> &'static str {
    match platform {
        ServerPlatform::WindowsX64 => "Users",
        ServerPlatform::LinuxX64 => "home",
    }
}

pub(super) fn profile_destination(
    profiles: &Path,
    platform: ServerPlatform,
    name: &str,
) -> PathBuf {
    profiles
        .join(profile_user_directory(platform))
        .join(name)
        .join(format!("{name}.Arma3Profile"))
}

fn check_profile_source(
    record_root: &Path,
    name: &str,
    source: &Path,
) -> Result<(), ServerStoreError> {
    if !crate::core::server::profile_name_valid(name) {
        return Err(ServerStoreError::UnsafePath);
    }
    let file_name = format!("{name}.Arma3Profile");
    let parts = source
        .strip_prefix(record_root)
        .map_err(|_| ServerStoreError::UnsafePath)?
        .components()
        .collect::<Vec<_>>();
    let normal = |index: usize| match parts.get(index) {
        Some(Component::Normal(value)) => value.to_str(),
        _ => None,
    };
    if parts.len() != 4
        || normal(0) != Some("runtimes")
        || !normal(1).is_some_and(|runtime| runtime.starts_with("runtime_"))
        || normal(2) != Some("configuration")
        || normal(3) != Some(file_name.as_str())
    {
        return Err(ServerStoreError::UnsafePath);
    }
    crate::core::server::store::check_directory_chain(
        source.parent().ok_or(ServerStoreError::UnsafePath)?,
    )
}

fn read_profile(
    source: &Path,
    expected_hash: &str,
    control: &ContentStageControl,
) -> Result<Vec<u8>, ServerStoreError> {
    crate::core::server::store::check_regular_file(source)?;
    let bytes = crate::core::server::content_staging::read_bounded_controlled(
        source,
        MAX_PROFILE_BYTES,
        control,
    )?;
    crate::core::server::store::check_regular_file(source)?;
    if blake3::hash(&bytes).to_hex().as_str() != expected_hash {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(bytes)
}

fn ensure_private_directory(path: &Path) -> Result<(), ServerStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(ServerStoreError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            builder.create(path).map_err(|_| ServerStoreError::Io)?;
        }
        Err(_) => return Err(ServerStoreError::Io),
    }
    crate::core::server::store::check_directory_chain(path)
}

fn replace_profile(
    destination: &Path,
    bytes: &[u8],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    control.checkpoint()?;
    let parent = destination.parent().ok_or(ServerStoreError::UnsafePath)?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            crate::core::server::store::check_regular_file(destination)?;
        }
        Ok(_) => return Err(ServerStoreError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ServerStoreError::Io),
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".profile-")
        .tempfile_in(parent)
        .map_err(|_| ServerStoreError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerStoreError::Io)?;
    }
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    control.checkpoint()?;
    temporary
        .persist(destination)
        .map_err(|_| ServerStoreError::Io)?;
    Ok(())
}

fn seed_profile(
    profiles: &Path,
    destination: &Path,
    name: &str,
    bytes: &[u8],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    control.checkpoint()?;
    crate::core::server::store::check_directory_chain(profiles)?;
    let users = [ServerPlatform::WindowsX64, ServerPlatform::LinuxX64]
        .into_iter()
        .find(|platform| profile_destination(profiles, *platform, name) == destination)
        .map(profile_user_directory)
        .ok_or(ServerStoreError::UnsafePath)?;
    ensure_private_directory(&profiles.join(users))?;
    ensure_private_directory(&profiles.join(users).join(name))?;
    replace_profile(destination, bytes, control)
}

impl ServerStore {
    /// Replaces only the engine's `<name>.Arma3Profile` inside the instance's owned runtime
    /// state. The engine's `.vars` namespace files, other profiles and logs stay untouched.
    pub(in crate::core::server) fn prepare_native_profile(
        &self,
        id: &ServerInstanceId,
        seed: &NativeProfileSeed,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        let record_root = self.record_root(id);
        check_profile_source(&record_root, &seed.profile_name, &seed.source)?;
        super::launch::validate_runtime_state(self, id, control)?;
        let profiles = record_root.join("runtime-state/home/profiles");
        let bytes = read_profile(&seed.source, &seed.blake3, control)?;
        seed_profile(
            &profiles,
            &seed.destination,
            &seed.profile_name,
            &bytes,
            control,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_replaces_only_the_engine_profile_file() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = temp.path().join("profiles");
        fs::create_dir(&profiles).unwrap();
        let control = ContentStageControl::default();
        for platform in [ServerPlatform::WindowsX64, ServerPlatform::LinuxX64] {
            let destination = profile_destination(&profiles, platform, "chosen");
            seed_profile(&profiles, &destination, "chosen", b"reviewed", &control).unwrap();
            fs::write(&destination, b"engine rewrite").unwrap();
            let user = destination.parent().unwrap();
            let vars = user.join("chosen.vars.Arma3Profile");
            fs::write(&vars, b"namespace").unwrap();
            let save = user.join("saves/mission.vars");
            fs::create_dir_all(save.parent().unwrap()).unwrap();
            fs::write(&save, b"save").unwrap();
            let other = user.parent().unwrap().join("Other/Other.Arma3Profile");
            fs::create_dir_all(other.parent().unwrap()).unwrap();
            fs::write(&other, b"other").unwrap();
            seed_profile(&profiles, &destination, "chosen", b"reviewed", &control).unwrap();
            assert_eq!(fs::read(&destination).unwrap(), b"reviewed");
            assert_eq!(fs::read(&vars).unwrap(), b"namespace");
            assert_eq!(fs::read(&save).unwrap(), b"save");
            assert_eq!(fs::read(&other).unwrap(), b"other");
            assert_eq!(
                fs::read_dir(user)
                    .unwrap()
                    .filter(|entry| entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".profile-"))
                    .count(),
                0
            );
        }
        assert_eq!(
            profile_destination(&profiles, ServerPlatform::LinuxX64, "chosen"),
            profiles.join("home/chosen/chosen.Arma3Profile")
        );
    }

    #[test]
    fn foreign_destinations_sources_and_stale_hashes_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = temp.path().join("profiles");
        fs::create_dir(&profiles).unwrap();
        let control = ContentStageControl::default();
        for destination in [
            profiles.join("Users/other/chosen.Arma3Profile"),
            profiles.join("Users/chosen/chosen.vars.Arma3Profile"),
            profiles.join("chosen.Arma3Profile"),
        ] {
            assert_eq!(
                seed_profile(&profiles, &destination, "chosen", b"x", &control),
                Err(ServerStoreError::UnsafePath)
            );
        }
        let destination = profile_destination(&profiles, ServerPlatform::WindowsX64, "chosen");
        fs::create_dir_all(&destination).unwrap();
        assert_eq!(
            seed_profile(&profiles, &destination, "chosen", b"x", &control),
            Err(ServerStoreError::UnsafePath)
        );

        let record = temp.path().join("record");
        let source = record.join("runtimes/runtime_a/configuration/chosen.Arma3Profile");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"actual").unwrap();
        assert!(check_profile_source(&record, "chosen", &source).is_ok());
        assert!(check_profile_source(&record, "other", &source).is_err());
        assert!(
            check_profile_source(
                &record,
                "chosen",
                &record.join("runtime-state/home/profiles/chosen.Arma3Profile")
            )
            .is_err()
        );
        let expected = blake3::hash(b"expected").to_hex().to_string();
        assert_eq!(
            read_profile(&source, &expected, &control).unwrap_err(),
            ServerStoreError::StaleRevision
        );
        let actual = blake3::hash(b"actual").to_hex().to_string();
        assert_eq!(read_profile(&source, &actual, &control).unwrap(), b"actual");
    }

    #[cfg(unix)]
    #[test]
    fn linked_profile_directories_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let profiles = temp.path().join("profiles");
        fs::create_dir(&profiles).unwrap();
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, profiles.join("home")).unwrap();
        let destination = profile_destination(&profiles, ServerPlatform::LinuxX64, "chosen");
        assert!(
            seed_profile(
                &profiles,
                &destination,
                "chosen",
                b"x",
                &ContentStageControl::default()
            )
            .is_err()
        );
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }
}
