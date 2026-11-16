use super::*;

#[derive(Default)]
struct ScanTotals {
    bytes: u64,
    path_bytes: usize,
}
pub(in crate::core::server) fn inventory(
    game: ServerDefinitionKind,
    source: &Path,
    platform: ServerPlatform,
    control: &ContentStageControl,
) -> Result<PayloadInventory, ServerStoreError> {
    checked_dir(source)?;
    let mut selected = BTreeMap::<String, StageEntryKind>::new();
    let executable = super::super::installation::executable_name(game, platform);
    for root in [executable, "steam_appid.txt"] {
        let path = source.join(root);
        if fs::symlink_metadata(&path).is_ok() {
            selected.insert(root.into(), StageEntryKind::File);
        }
    }
    let mut scanned = 0usize;
    let mut root_path_bytes = 0usize;
    for entry in fs::read_dir(source).map_err(|_| ServerStoreError::Io)? {
        scanned += 1;
        if scanned > MAX_ENTRIES {
            return Err(ServerStoreError::Invalid(
                "Native source root exceeds entry limit".into(),
            ));
        }
        let entry = entry.map_err(|_| ServerStoreError::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ServerStoreError::UnsafePath)?;
        root_path_bytes = root_path_bytes.saturating_add(name.len());
        if name.len() > MAX_PATH_BYTES || root_path_bytes > MAX_PATH_BYTES_TOTAL {
            return Err(ServerStoreError::Invalid(
                "Native source root exceeds path limit".into(),
            ));
        }
        let library_name = match platform {
            ServerPlatform::WindowsX64 => name.to_lowercase().ends_with(".dll"),
            ServerPlatform::LinuxX64 => is_linux_library(&name),
        };
        if library_name {
            selected.insert(name, StageEntryKind::File);
        } else if selected_directories(game)
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&name))
        {
            selected.insert(name, StageEntryKind::Directory);
        }
        if selected.len() > MAX_ENTRIES {
            return Err(ServerStoreError::Invalid(
                "Native payload exceeds entry limit".into(),
            ));
        }
    }
    let mut entries = Vec::new();
    let mut totals = ScanTotals::default();
    for (name, kind) in selected {
        walk(source, &name, kind, 0, &mut entries, &mut totals, control)?;
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let inventory = PayloadInventory {
        platform,
        entries,
        total_bytes: totals.bytes,
    };
    validate_inventory(game, &inventory)?;
    Ok(inventory)
}

fn walk(
    root: &Path,
    rel: &str,
    kind: StageEntryKind,
    depth: usize,
    out: &mut Vec<PayloadEntry>,
    totals: &mut ScanTotals,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    control.checkpoint()?;
    if depth > 16 || out.len() >= MAX_ENTRIES || rel.len() > MAX_PATH_BYTES {
        return Err(ServerStoreError::Invalid(
            "Native payload exceeds bounds".into(),
        ));
    }
    let relative = parse_relative(rel)?;
    totals.path_bytes = totals
        .path_bytes
        .checked_add(rel.len())
        .ok_or(ServerStoreError::Corrupt)?;
    if totals.path_bytes > MAX_PATH_BYTES_TOTAL {
        return Err(ServerStoreError::Invalid(
            "Native payload paths exceed byte limit".into(),
        ));
    }
    let path = root.join(relative);
    match kind {
        StageEntryKind::Directory => {
            checked_dir(&path)?;
            out.push(PayloadEntry {
                path: rel.into(),
                kind,
                bytes: 0,
                blake3: None,
                executable: false,
            });
            let mut children = Vec::new();
            for child in fs::read_dir(path).map_err(|_| ServerStoreError::Io)? {
                if children.len() >= MAX_ENTRIES
                    || out.len().saturating_add(children.len()) >= MAX_ENTRIES
                {
                    return Err(ServerStoreError::Invalid(
                        "Native payload exceeds entry limit".into(),
                    ));
                }
                children.push(child.map_err(|_| ServerStoreError::Io)?);
            }
            children.sort_by_key(|e| e.file_name());
            for child in children {
                let child_name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| ServerStoreError::UnsafePath)?;
                let child_path = format!("{rel}/{child_name}");
                let ty = child.file_type().map_err(|_| ServerStoreError::Io)?;
                let child_kind = if ty.is_dir() {
                    StageEntryKind::Directory
                } else {
                    StageEntryKind::File
                };
                walk(
                    root,
                    &child_path,
                    child_kind,
                    depth + 1,
                    out,
                    totals,
                    control,
                )?;
            }
        }
        StageEntryKind::File => {
            let meta = checked_file(&path)?;
            if meta.len() > MAX_FILE_BYTES {
                return Err(ServerStoreError::Invalid(
                    "Native payload file exceeds size limit".into(),
                ));
            }
            let file = open_read(&path)?;
            let mut reader = file.take(meta.len().saturating_add(1));
            let mut hasher = blake3::Hasher::new();
            let mut buf = [0; BUFFER_SIZE];
            let mut bytes = 0u64;
            loop {
                control.checkpoint()?;
                let n = reader.read(&mut buf).map_err(|_| ServerStoreError::Io)?;
                if n == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(n as u64)
                    .ok_or(ServerStoreError::Invalid(
                        "Native payload size overflow".into(),
                    ))?;
                if bytes > meta.len() || bytes > MAX_FILE_BYTES {
                    return Err(ServerStoreError::StaleRevision);
                }
                hasher.update(&buf[..n]);
            }
            if bytes != meta.len() {
                return Err(ServerStoreError::StaleRevision);
            }
            totals.bytes = totals
                .bytes
                .checked_add(bytes)
                .ok_or(ServerStoreError::Invalid(
                    "Native payload size overflow".into(),
                ))?;
            out.push(PayloadEntry {
                path: rel.into(),
                kind,
                bytes,
                blake3: Some(hasher.finalize().to_hex().to_string()),
                executable: executable(&meta),
            });
        }
    }
    Ok(())
}
