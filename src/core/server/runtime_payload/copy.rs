use super::*;
pub(in crate::core::server) fn copy(
    game: ServerDefinitionKind,
    source: &Path,
    destination: &Path,
    inventory: &PayloadInventory,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    validate_inventory(game, inventory)?;
    checked_dir(source)?;
    let source_real = source
        .canonicalize()
        .map_err(|_| ServerStoreError::UnsafePath)?;
    let destination_parent = destination.parent().ok_or(ServerStoreError::UnsafePath)?;
    super::store::check_directory_chain(destination_parent)?;
    let parent_real = destination_parent
        .canonicalize()
        .map_err(|_| ServerStoreError::UnsafePath)?;
    let destination_real = parent_real.join(
        destination
            .file_name()
            .ok_or(ServerStoreError::UnsafePath)?,
    );
    if source_real.starts_with(&destination_real)
        || destination_real.starts_with(&source_real)
        || destination.exists()
    {
        return Err(ServerStoreError::UnsafePath);
    }
    fs::create_dir(destination).map_err(|_| ServerStoreError::Io)?;
    for e in &inventory.entries {
        control.checkpoint()?;
        let rel = parse_relative(&e.path)?;
        let src = source.join(&rel);
        let dst = destination.join(&rel);
        match e.kind {
            StageEntryKind::Directory => {
                checked_dir(&src)?;
                fs::create_dir(&dst).map_err(|_| ServerStoreError::Io)?;
            }
            StageEntryKind::File => {
                checked_file(&src)?;
                if let Some(p) = dst.parent() {
                    super::store::check_directory_chain(p)?;
                }
                copy_file(&src, &dst, e, control)?;
            }
        }
    }
    verify_selected(game, destination, inventory, control)
}

pub(in crate::core::server) fn verify_selected(
    game: ServerDefinitionKind,
    root: &Path,
    inventory: &PayloadInventory,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    validate_inventory(game, inventory)?;
    checked_dir(root)?;
    for e in &inventory.entries {
        control.checkpoint()?;
        let p = root.join(parse_relative(&e.path)?);
        match e.kind {
            StageEntryKind::Directory => {
                checked_dir(&p)?;
            }
            StageEntryKind::File => {
                let m = checked_file(&p)?;
                if m.len() != e.bytes
                    || executable(&m) != e.executable
                    || digest(&p, e.bytes, control)? != e.blake3
                {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
        }
    }
    Ok(())
}
