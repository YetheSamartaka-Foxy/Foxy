use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn publish<T>(output_dir: &Path, build: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let output = std::path::absolute(output_dir)?;
    let parent = output.parent().context("Output has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    if let Ok(meta) = std::fs::symlink_metadata(&output)
        && (meta.file_type().is_symlink() || !meta.is_dir())
    {
        bail!(
            "Atomic output must be a real directory: {}",
            output.display()
        );
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .context("Output needs a valid directory name")?;
    let stage = parent.join(format!(".{name}.foxy-stage-{}-{nonce}", std::process::id()));
    let backup = parent.join(format!(
        ".{name}.foxy-backup-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&stage)
        .with_context(|| format!("Failed to create stage {}", stage.display()))?;
    let result = match build(&stage) {
        Ok(result) => result,
        Err(err) => {
            remove_generated_dir(&stage, parent).ok();
            return Err(err);
        }
    };
    let had_output = output.exists();
    if had_output {
        std::fs::rename(&output, &backup)
            .with_context(|| format!("Failed to move old output {}", output.display()))?;
    }
    if let Err(err) = std::fs::rename(&stage, &output) {
        if had_output {
            std::fs::rename(&backup, &output).ok();
        }
        remove_generated_dir(&stage, parent).ok();
        return Err(err).with_context(|| format!("Failed to publish {}", output.display()));
    }
    if had_output {
        remove_generated_dir(&backup, parent).with_context(|| {
            format!(
                "Published output, but failed to remove {}",
                backup.display()
            )
        })?;
    }
    crate::output::replace_output_path(&output);
    println!("Published staged output to {}", output.display());
    Ok(result)
}

fn remove_generated_dir(path: &Path, parent: &Path) -> Result<()> {
    let parent = std::fs::canonicalize(parent)?;
    let target = std::fs::canonicalize(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || target.parent() != Some(parent.as_path())
    {
        bail!(
            "Refusing to remove unexpected staging path {}",
            path.display()
        );
    }
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_build_preserves_old_output() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("old.txt"), b"old").unwrap();
        let result: Result<()> = publish(&output, |stage| {
            std::fs::write(stage.join("new.txt"), b"new")?;
            bail!("build failed")
        });
        assert!(result.is_err());
        assert!(output.join("old.txt").exists());
        assert!(!output.join("new.txt").exists());
    }
}
