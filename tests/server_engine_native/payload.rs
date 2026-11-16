use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
pub(super) type Inventory = BTreeMap<PathBuf, (u64, blake3::Hash)>;
pub(super) fn ordinary(path: &Path, directory: bool) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let link = metadata.is_symlink();
    if link || (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(io::Error::other(
            "Native payload must contain ordinary entries",
        ));
    }
    Ok(metadata)
}

fn digest(path: &Path) -> io::Result<(u64, blake3::Hash)> {
    let size = ordinary(path, false)?.len();
    if size > 8 * 1024 * 1024 * 1024 {
        return Err(io::Error::other("Native input exceeds bounded file size"));
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut reader = Read::by_ref(&mut file).take(size + 1);
    let mut buffer = vec![0; 1024 * 1024];
    let mut read = 0;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        hasher.update(&buffer[..count]);
    }
    if read != size || file.metadata()?.len() != size {
        return Err(io::Error::other("Native payload changed while reading"));
    }
    Ok((size, hasher.finalize()))
}

pub(super) fn copy_payload(
    source: &Path,
    destination: &Path,
    reuse: bool,
) -> io::Result<Inventory> {
    ordinary(&source.join("steam_appid.txt"), false)?;
    let mut identity = vec![];
    fs::File::open(source.join("steam_appid.txt"))?
        .take(65)
        .read_to_end(&mut identity)?;
    if identity.len() > 64 || String::from_utf8_lossy(&identity).trim() != "107410" {
        return Err(io::Error::other(
            "Native payload has an unexpected Steam game identity",
        ));
    }
    if reuse {
        ordinary(destination, true)?;
    } else {
        fs::create_dir(destination)?;
    }
    let mut inventory = Inventory::new();
    #[cfg(windows)]
    let executable = "arma3server_x64.exe";
    #[cfg(target_os = "linux")]
    let executable = "arma3server_x64";
    copy_file(
        source,
        destination,
        Path::new(executable),
        &mut inventory,
        reuse,
    )?;
    for (index, entry) in fs::read_dir(source)?.enumerate() {
        if index >= 100_000 {
            return Err(io::Error::other(
                "Native inventory exceeds root entry count",
            ));
        }
        let entry = entry?;
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|part| part.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("dll") || extension == "so" {
            copy_file(
                source,
                destination,
                Path::new(&entry.file_name()),
                &mut inventory,
                reuse,
            )?;
        }
    }
    for name in [
        "Addons",
        "Dta",
        "Dll",
        "linux64",
        "Keys",
        "Expansion",
        "AoW",
        "Argo",
        "Curator",
        "Enoch",
        "Heli",
        "Jets",
        "Kart",
        "Mark",
        "Orange",
        "Tacops",
        "Tank",
    ] {
        let mut matching = None;
        for (index, entry) in fs::read_dir(source)?.enumerate() {
            if index >= 100_000 {
                return Err(io::Error::other(
                    "Native inventory exceeds root entry count",
                ));
            }
            let entry = entry?;
            if entry.file_name().eq_ignore_ascii_case(name) {
                if matching.is_some() {
                    return Err(io::Error::other("Ambiguous native payload folder"));
                }
                matching = Some(entry);
            }
        }
        if let Some(entry) = matching {
            copy_directory(
                source,
                destination,
                Path::new(&entry.file_name()),
                &mut inventory,
                0,
                reuse,
            )?;
        }
    }
    let app_id = Path::new("steam_appid.txt");
    if reuse && !destination.join(app_id).try_exists()? {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(app_id))?;
        file.write_all(&identity)?;
        file.sync_all()?;
    }
    copy_file(source, destination, app_id, &mut inventory, reuse)?;
    println!("Verified {} ordinary source payload files", inventory.len());
    Ok(inventory)
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    relative: &Path,
    inventory: &mut Inventory,
    depth: usize,
    reuse: bool,
) -> io::Result<()> {
    if depth > 16 || inventory.len() > 100_000 {
        return Err(io::Error::other("Native inventory exceeds bounds"));
    }
    ordinary(&source.join(relative), true)?;
    if reuse && destination.join(relative).try_exists()? {
        ordinary(&destination.join(relative), true)?;
    } else {
        fs::create_dir(destination.join(relative))?;
    }
    for entry in fs::read_dir(source.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            copy_directory(source, destination, &child, inventory, depth + 1, reuse)?;
        } else {
            copy_file(source, destination, &child, inventory, reuse)?;
        }
    }
    Ok(())
}

fn copy_file(
    source: &Path,
    destination: &Path,
    relative: &Path,
    inventory: &mut Inventory,
    reuse: bool,
) -> io::Result<()> {
    if inventory.len() >= 100_000 {
        return Err(io::Error::other("Native inventory exceeds file count"));
    }
    let before = digest(&source.join(relative))?;
    if reuse && !destination.join(relative).try_exists()? {
        let mut input = fs::File::open(source.join(relative))?.take(before.0 + 1);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(relative))?;
        let mut buffer = vec![0; 1024 * 1024];
        let mut written = 0;
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            written += count as u64;
        }
        output.sync_all()?;
        if written != before.0 {
            return Err(io::Error::other("Native source changed during copy"));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = ordinary(&source.join(relative), false)?
                .permissions()
                .mode();
            output.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    if (!reuse && fs::copy(source.join(relative), destination.join(relative))? != before.0)
        || digest(&destination.join(relative))? != before
    {
        return Err(io::Error::other("Native copy verification failed"));
    }
    inventory.insert(relative.to_path_buf(), before);
    Ok(())
}

pub(super) fn verify_inventory(root: &Path, inventory: &Inventory) -> io::Result<()> {
    for (relative, expected) in inventory {
        if digest(&root.join(relative))? != *expected {
            return Err(io::Error::other("Native payload changed"));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn copied_linux_payload_includes_the_executable_rpath_libraries() -> io::Result<()> {
    let source = tempfile::tempdir()?;
    let scratch = tempfile::tempdir()?;
    fs::write(source.path().join("steam_appid.txt"), b"107410\n")?;
    fs::write(source.path().join("arma3server_x64"), b"native fixture")?;
    fs::create_dir(source.path().join("linux64"))?;
    fs::write(
        source.path().join("linux64/libsteam_api.so"),
        b"fixture library",
    )?;
    let destination = scratch.path().join("payload");
    let inventory = copy_payload(source.path(), &destination, false)?;
    assert_eq!(
        fs::read(destination.join("linux64/libsteam_api.so"))?,
        b"fixture library"
    );
    assert!(inventory.contains_key(Path::new("linux64/libsteam_api.so")));
    verify_inventory(source.path(), &inventory)?;
    verify_inventory(&destination, &inventory)
}

#[cfg(target_os = "linux")]
#[test]
fn filling_a_missing_prepared_executable_preserves_execute_permissions() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let relative = Path::new("arma3server_x64");
    fs::write(source.path().join(relative), b"native fixture")?;
    fs::set_permissions(
        source.path().join(relative),
        fs::Permissions::from_mode(0o751),
    )?;
    let mut inventory = Inventory::new();
    copy_file(
        source.path(),
        destination.path(),
        relative,
        &mut inventory,
        true,
    )?;
    assert_eq!(
        fs::metadata(destination.path().join(relative))?
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    verify_inventory(source.path(), &inventory)?;
    fs::write(destination.path().join(relative), b"existing private data")?;
    assert!(
        copy_file(
            source.path(),
            destination.path(),
            relative,
            &mut inventory,
            true
        )
        .is_err()
    );
    assert_eq!(
        fs::read(destination.path().join(relative))?,
        b"existing private data"
    );
    Ok(())
}
