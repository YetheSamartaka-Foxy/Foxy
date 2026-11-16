use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

type Environment = Vec<(OsString, OsString)>;

pub(super) fn prepare_home(
    instance: &Path,
    profiles: &Path,
) -> io::Result<(Environment, Vec<PathBuf>)> {
    #[cfg(target_os = "linux")]
    {
        let home = instance.join("home");
        fs::create_dir(&home)?;
        let data = home.join(".local/share");
        let config = home.join(".config");
        let cache = home.join(".cache");
        let runtime = home.join("runtime");
        for directory in [&data, &config, &cache, &runtime] {
            fs::create_dir_all(directory)?;
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
        let mut reports = vec![profiles.to_path_buf()];
        for name in ["Arma 3", "Arma 3 - Other Profiles"] {
            let directory = data.join(name);
            fs::create_dir(&directory)?;
            reports.push(directory);
        }
        Ok((
            [
                ("HOME", home),
                ("XDG_DATA_HOME", data),
                ("XDG_CONFIG_HOME", config),
                ("XDG_CACHE_HOME", cache),
                ("XDG_RUNTIME_DIR", runtime),
            ]
            .into_iter()
            .map(|(name, path)| (OsString::from(name), path.into_os_string()))
            .collect(),
            reports,
        ))
    }
    #[cfg(windows)]
    {
        let _ = instance;
        Ok((vec![], vec![profiles.to_path_buf()]))
    }
}

pub(super) fn prepare_shared_directories(
    payload: &Path,
    run: &OsStr,
    name: &str,
) -> io::Result<()> {
    let mut directory = payload.to_path_buf();
    for component in [OsStr::new("private-instances"), run] {
        directory.push(component);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                super::payload::ordinary(&directory, true)?;
            }
            Err(error) => return Err(error),
        }
    }
    directory.push(name);
    fs::create_dir(&directory)?;
    fs::create_dir(directory.join("missions"))?;
    fs::create_dir(directory.join("keys"))
}

#[test]
fn shared_runtime_directories_are_new_and_separate_for_each_instance() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    for name in ["alpha", "beta"] {
        prepare_shared_directories(root.path(), OsStr::new("run"), name)?;
        let instance = root.path().join("private-instances/run").join(name);
        assert!(instance.join("missions").is_dir());
        assert!(instance.join("keys").is_dir());
    }
    assert!(prepare_shared_directories(root.path(), OsStr::new("run"), "alpha").is_err());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_homes_redirect_default_paths_into_separate_instance_directories() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let mut environments = vec![];
    for name in ["alpha", "beta"] {
        let instance = root.path().join(name);
        let profiles = instance.join("profiles");
        fs::create_dir_all(&profiles)?;
        let (environment, reports) = prepare_home(&instance, &profiles)?;
        assert_eq!(environment.len(), 5);
        for (_, value) in &environment {
            let path = Path::new(value);
            assert!(path.starts_with(&instance));
            assert!(path.is_dir());
        }
        for directory in reports {
            assert!(directory.starts_with(&instance));
            assert!(directory.is_dir());
        }
        assert_eq!(
            fs::metadata(instance.join("home/runtime"))?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        environments.push(environment);
    }
    assert_ne!(environments[0], environments[1]);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn prepared_runtime_links_are_rejected_before_external_writes() -> io::Result<()> {
    use std::os::unix::fs::symlink;
    for component in ["private-instances", "private-instances/run"] {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let link = root.path().join(component);
        fs::create_dir_all(link.parent().unwrap())?;
        symlink(outside.path(), link)?;
        assert!(prepare_shared_directories(root.path(), OsStr::new("run"), "alpha").is_err());
        assert!(fs::read_dir(outside.path())?.next().is_none());
    }
    Ok(())
}
