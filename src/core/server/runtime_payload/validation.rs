use super::*;
pub(super) fn validate_inventory(
    game: ServerDefinitionKind,
    i: &PayloadInventory,
) -> Result<(), ServerStoreError> {
    if i.entries.len() > MAX_ENTRIES {
        return Err(ServerStoreError::Corrupt);
    }
    let executable = super::super::installation::executable_name(game, i.platform);
    let mut paths = std::collections::BTreeSet::new();
    let mut kinds = BTreeMap::<String, StageEntryKind>::new();
    let mut total = 0u64;
    let mut path_bytes = 0usize;
    let mut prev = None;
    let mut has_executable = false;
    for e in &i.entries {
        let rel = parse_relative(&e.path).map_err(|_| ServerStoreError::Corrupt)?;
        if e.path.len() > MAX_PATH_BYTES || rel.components().count() > 17 {
            return Err(ServerStoreError::Corrupt);
        }
        path_bytes = path_bytes
            .checked_add(e.path.len())
            .ok_or(ServerStoreError::Corrupt)?;
        if path_bytes > MAX_PATH_BYTES_TOTAL {
            return Err(ServerStoreError::Corrupt);
        }
        let first = rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or_default()
            .to_lowercase();
        let allowed_root = first == executable.to_ascii_lowercase()
            || first == "steam_appid.txt"
            || selected_directories(game).contains(&first.as_str())
            || match i.platform {
                ServerPlatform::WindowsX64 => first.ends_with(".dll"),
                ServerPlatform::LinuxX64 => is_linux_library(&first),
            };
        if !allowed_root {
            return Err(ServerStoreError::Corrupt);
        }
        if prev.as_ref().is_some_and(|p: &String| p >= &e.path)
            || !paths.insert(
                e.path
                    .chars()
                    .flat_map(char::to_uppercase)
                    .collect::<String>(),
            )
        {
            return Err(ServerStoreError::Corrupt);
        }
        prev = Some(e.path.clone());
        if e.path.eq_ignore_ascii_case(executable) && e.kind == StageEntryKind::File {
            has_executable = true;
        }
        kinds.insert(
            e.path
                .chars()
                .flat_map(char::to_uppercase)
                .collect::<String>(),
            e.kind,
        );
        match e.kind {
            StageEntryKind::Directory if e.bytes == 0 && e.blake3.is_none() && !e.executable => {}
            StageEntryKind::File
                if e.bytes <= MAX_FILE_BYTES && e.blake3.as_deref().is_some_and(valid_hash) =>
            {
                total = total
                    .checked_add(e.bytes)
                    .ok_or(ServerStoreError::Corrupt)?;
                if total > MAX_FILE_BYTES.saturating_mul(MAX_ENTRIES as u64) {
                    return Err(ServerStoreError::Corrupt);
                }
            }
            _ => return Err(ServerStoreError::Corrupt),
        }
    }
    if total != i.total_bytes || !has_executable {
        return Err(ServerStoreError::Corrupt);
    }
    for e in &i.entries {
        let mut parent = Path::new(&e.path).parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            let key = p
                .to_string_lossy()
                .chars()
                .flat_map(char::to_uppercase)
                .collect::<String>();
            if kinds.get(&key) != Some(&StageEntryKind::Directory) {
                return Err(ServerStoreError::Corrupt);
            }
            parent = p.parent();
        }
    }
    Ok(())
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn checked_dir(p: &Path) -> Result<(), ServerStoreError> {
    super::store::check_directory_chain(p)?;
    let m = fs::symlink_metadata(p).map_err(|_| ServerStoreError::Io)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}
pub(super) fn checked_file(p: &Path) -> Result<fs::Metadata, ServerStoreError> {
    super::store::check_directory_chain(p.parent().ok_or(ServerStoreError::UnsafePath)?)?;
    let m = fs::symlink_metadata(p).map_err(|_| ServerStoreError::Io)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(m)
}
pub(super) fn open_read(p: &Path) -> Result<File, ServerStoreError> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    o.open(p).map_err(|_| ServerStoreError::UnsafePath)
}
pub(super) fn executable(_metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}
pub(super) fn digest(
    p: &Path,
    expected: u64,
    c: &ContentStageControl,
) -> Result<Option<String>, ServerStoreError> {
    let f = open_read(p)?;
    let mut f = f.take(expected.saturating_add(1));
    let mut h = blake3::Hasher::new();
    let mut b = [0; BUFFER_SIZE];
    let mut bytes = 0u64;
    loop {
        c.checkpoint()?;
        let n = f.read(&mut b).map_err(|_| ServerStoreError::Io)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or(ServerStoreError::Corrupt)?;
        if bytes > expected || bytes > MAX_FILE_BYTES {
            return Err(ServerStoreError::Corrupt);
        }
        h.update(&b[..n]);
    }
    if bytes != expected {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(Some(h.finalize().to_hex().to_string()))
}
pub(super) fn copy_file(
    src: &Path,
    dst: &Path,
    e: &PayloadEntry,
    c: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let input = open_read(src)?;
    let mut input = input.take(e.bytes.saturating_add(1));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o644);
    }
    let mut output = opts.open(dst).map_err(|_| ServerStoreError::Io)?;
    let mut h = blake3::Hasher::new();
    let mut b = [0; BUFFER_SIZE];
    let mut bytes = 0u64;
    loop {
        c.checkpoint()?;
        let n = input.read(&mut b).map_err(|_| ServerStoreError::Io)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or(ServerStoreError::StaleRevision)?;
        if bytes > e.bytes || bytes > MAX_FILE_BYTES {
            return Err(ServerStoreError::StaleRevision);
        }
        h.update(&b[..n]);
        output
            .write_all(&b[..n])
            .map_err(|_| ServerStoreError::Io)?;
        c.record_bytes(n as u64);
    }
    if bytes != e.bytes || Some(h.finalize().to_hex().to_string()) != e.blake3 {
        return Err(ServerStoreError::StaleRevision);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dst,
            fs::Permissions::from_mode(if e.executable { 0o755 } else { 0o644 }),
        )
        .map_err(|_| ServerStoreError::Io)?;
    }
    output.sync_all().map_err(|_| ServerStoreError::Io)?;
    c.record_file();
    Ok(())
}
