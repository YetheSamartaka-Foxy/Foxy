use anyhow::{Context, Result};
use indicatif::ProgressBar;
use md5::{Digest as Md5Digest, Md5};
use rayon::prelude::*;
use sha1::Sha1;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::GenerationMode;
use crate::pbo;
use crate::types::{Checksums, DiscoveredFile, FilePart, ModFile, ProcessedMod, ResolvedMod};

const BUF_SIZE_SWIFTY: usize = 256 * 1024;
const BUF_SIZE_FOXY: usize = 1024 * 1024; // BLAKE3 benefits from larger buffers

/// Unified hasher wrapping either MD5 or BLAKE3.
enum FlexHasher {
    Md5(Md5),
    Blake3(Box<blake3::Hasher>),
}

impl FlexHasher {
    fn new_md5() -> Self {
        FlexHasher::Md5(Md5::new())
    }
    fn new_blake3() -> Self {
        FlexHasher::Blake3(Box::new(blake3::Hasher::new()))
    }
    fn update(&mut self, data: &[u8]) {
        match self {
            FlexHasher::Md5(h) => h.update(data),
            FlexHasher::Blake3(h) => {
                h.update(data);
            }
        }
    }
    fn finalize_hex(self) -> String {
        match self {
            FlexHasher::Md5(h) => hex::encode_upper(h.finalize()),
            FlexHasher::Blake3(h) => h.finalize().to_hex().to_uppercase(),
        }
    }
}

fn buf_size_for_mode(mode: GenerationMode) -> usize {
    match mode {
        GenerationMode::Swifty => BUF_SIZE_SWIFTY,
        GenerationMode::Foxy | GenerationMode::Hybrid => BUF_SIZE_FOXY,
    }
}

/// Assemble a `Checksums` from optional MD5 and BLAKE3 values based on mode.
fn assemble_checksums(
    md5: Option<String>,
    blake3: Option<String>,
    mode: GenerationMode,
) -> Checksums {
    match mode {
        GenerationMode::Swifty => Checksums::Md5(md5.expect("md5 required for SwiftyMode")),
        GenerationMode::Foxy => Checksums::Blake3(blake3.expect("blake3 required for FoxyMode")),
        GenerationMode::Hybrid => Checksums::Hybrid {
            md5: md5.expect("md5 required for HybridMode"),
            blake3: blake3.expect("blake3 required for HybridMode"),
        },
    }
}

fn finalize_checksums(
    md5: Option<FlexHasher>,
    blake3: Option<FlexHasher>,
    mode: GenerationMode,
) -> Checksums {
    assemble_checksums(
        md5.map(|h| h.finalize_hex()),
        blake3.map(|h| h.finalize_hex()),
        mode,
    )
}

/// A file work item bundled with its mod context.
struct FileWorkItem {
    file: DiscoveredFile,
    mod_index: usize,
    file_data_order: usize,
    output_path: std::path::PathBuf,
}

/// Process all mods: copy files to output, hash parts, compute checksums.
pub fn process_mods(
    resolved_mods: &[ResolvedMod],
    output_dir: &Path,
    progress: &ProgressBar,
    mode: GenerationMode,
) -> Result<Vec<ProcessedMod>> {
    let mut work_items: Vec<FileWorkItem> = Vec::new();

    for (mod_index, resolved) in resolved_mods.iter().enumerate() {
        let files = crate::discover::discover_files(&resolved.source_path)?;

        for (file_data_order, file) in files.into_iter().enumerate() {
            let output_path = output_dir
                .join(&resolved.mod_name)
                .join(&file.relative_path);
            work_items.push(FileWorkItem {
                file,
                mod_index,
                file_data_order,
                output_path,
            });
        }
    }

    progress.set_length(work_items.len() as u64);

    let dirs: std::collections::HashSet<_> = work_items
        .iter()
        .filter_map(|w| w.output_path.parent().map(|p| p.to_path_buf()))
        .collect();
    for dir in &dirs {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    }

    let progress = progress.clone();
    let results: Vec<Result<(usize, usize, ModFile)>> = work_items
        .into_par_iter()
        .map(|item| {
            let mod_file = copy_and_hash_file(
                &item.file.absolute_path,
                &item.output_path,
                &item.file,
                item.file_data_order,
                mode,
            )
            .with_context(|| format!("Failed to process {}", item.file.absolute_path.display()))?;
            progress.inc(1);
            Ok((item.mod_index, item.file_data_order, mod_file))
        })
        .collect();

    let results: Vec<(usize, usize, ModFile)> = results.into_iter().collect::<Result<Vec<_>>>()?;

    let mut mod_files: Vec<Vec<ModFile>> = vec![Vec::new(); resolved_mods.len()];
    for (mod_index, _order, mod_file) in results {
        mod_files[mod_index].push(mod_file);
    }

    for files in &mut mod_files {
        files.sort_by_key(|f| f.data_order);
    }

    let mut processed_mods = Vec::with_capacity(resolved_mods.len());
    for (mod_index, resolved) in resolved_mods.iter().enumerate() {
        let files = std::mem::take(&mut mod_files[mod_index]);
        let checksums = compute_mod_checksums(&files, mode);
        processed_mods.push(ProcessedMod {
            mod_name: resolved.mod_name.clone(),
            checksums,
            files,
            is_required: resolved.is_required,
            enabled: resolved.enabled,
            client_side: resolved.client_side,
        });
    }

    Ok(processed_mods)
}

fn compute_mod_checksums(files: &[ModFile], mode: GenerationMode) -> Checksums {
    let md5 = if matches!(mode, GenerationMode::Swifty | GenerationMode::Hybrid) {
        Some(compute_addon_checksum(FlexHasher::new_md5(), files, |f| {
            f.checksums.unwrap_md5()
        }))
    } else {
        None
    };
    let blake3 = if matches!(mode, GenerationMode::Foxy | GenerationMode::Hybrid) {
        Some(compute_parent_checksum(
            FlexHasher::new_blake3(),
            files
                .iter()
                .map(|f| (f.data_order, f.checksums.unwrap_blake3())),
        ))
    } else {
        None
    };
    assemble_checksums(md5, blake3, mode)
}

/// Compute the repository-level checksum (SHA-1, SwiftyMode-compatible).
pub fn compute_repo_checksum(mods: &[ProcessedMod]) -> String {
    compute_repo_checksum_for_ticks(mods, current_utc_ticks())
}

/// Compute the repository-level BLAKE3 checksum (FoxyMode).
pub fn compute_foxy_repo_checksum(mods: &[ProcessedMod]) -> String {
    let mut hasher = FlexHasher::new_blake3();
    for checksum in mods
        .iter()
        .filter(|m| m.is_required)
        .map(|m| m.checksums.unwrap_blake3())
    {
        hasher.update(checksum.as_bytes());
    }
    for checksum in mods
        .iter()
        .filter(|m| !m.is_required)
        .map(|m| m.checksums.unwrap_blake3())
    {
        hasher.update(checksum.as_bytes());
    }
    hasher.finalize_hex()
}

/// Compute SHA-1 hash of a file at the given path (for repo/icon images).
pub fn hash_file_sha1(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file for hashing: {}", path.display()))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; BUF_SIZE_SWIFTY];
    loop {
        let n = file.read(&mut buf).context("Failed to read file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()).to_uppercase())
}

/// Copy a file from source to destination while simultaneously hashing its parts.
/// In HybridMode, computes both MD5 and BLAKE3 in a single I/O pass.
fn copy_and_hash_file(
    source: &Path,
    dest: &Path,
    discovered: &DiscoveredFile,
    data_order: usize,
    mode: GenerationMode,
) -> Result<ModFile> {
    let use_md5 = matches!(mode, GenerationMode::Swifty | GenerationMode::Hybrid);
    let use_blake3 = matches!(mode, GenerationMode::Foxy | GenerationMode::Hybrid);
    let buf_size = buf_size_for_mode(mode);

    let mut parts = if pbo::is_pbo(source) {
        match pbo::parse_pbo_parts(source) {
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "PBO parse failed for {}, treating as single file: {}",
                    source.display(),
                    e
                );
                pbo::single_part(&discovered.relative_path, discovered.file_size)
            }
        }
    } else {
        pbo::single_part(&discovered.relative_path, discovered.file_size)
    };

    let mut src_file = std::fs::File::open(source)
        .with_context(|| format!("Failed to open source: {}", source.display()))?;
    let dst_file = std::fs::File::create(dest)
        .with_context(|| format!("Failed to create destination: {}", dest.display()))?;
    let mut writer = BufWriter::with_capacity(buf_size, dst_file);

    let mut buf = vec![0u8; buf_size];
    let mut file_position: u64 = 0;

    parts.sort_by_key(|p| p.start);

    for part in &mut parts {
        if file_position < part.start {
            let gap = part.start - file_position;
            copy_bytes(&mut src_file, &mut writer, &mut buf, gap)?;
            file_position += gap;
        }

        let mut md5_hasher = if use_md5 {
            Some(FlexHasher::new_md5())
        } else {
            None
        };
        let mut b3_hasher = if use_blake3 {
            Some(FlexHasher::new_blake3())
        } else {
            None
        };

        let mut remaining = part.length;
        while remaining > 0 {
            let to_read = (remaining as usize).min(buf_size);
            let n = src_file
                .read(&mut buf[..to_read])
                .context("Failed to read source file")?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n]).context("Failed to write")?;
            if let Some(ref mut h) = md5_hasher {
                h.update(&buf[..n]);
            }
            if let Some(ref mut h) = b3_hasher {
                h.update(&buf[..n]);
            }
            remaining -= n as u64;
            file_position += n as u64;
        }

        part.checksums = finalize_checksums(md5_hasher, b3_hasher, mode);
    }

    // Copy any remaining bytes after the last part
    loop {
        let n = src_file.read(&mut buf).context("Failed to read")?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).context("Failed to write")?;
    }

    writer.flush().context("Failed to flush output")?;

    let file_checksums = compute_file_checksums(&parts, mode);

    Ok(ModFile {
        relative_path: discovered.relative_path.clone(),
        checksums: file_checksums,
        length: discovered.file_size,
        parts,
        data_order,
    })
}

fn compute_file_checksums(parts: &[FilePart], mode: GenerationMode) -> Checksums {
    let md5 = if matches!(mode, GenerationMode::Swifty | GenerationMode::Hybrid) {
        Some(compute_parent_checksum(
            FlexHasher::new_md5(),
            parts
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.checksums.unwrap_md5())),
        ))
    } else {
        None
    };
    let blake3 = if matches!(mode, GenerationMode::Foxy | GenerationMode::Hybrid) {
        Some(compute_parent_checksum(
            FlexHasher::new_blake3(),
            parts
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.checksums.unwrap_blake3())),
        ))
    } else {
        None
    };
    assemble_checksums(md5, blake3, mode)
}

/// Copy exactly `count` bytes from reader to writer without hashing.
fn copy_bytes(
    reader: &mut std::fs::File,
    writer: &mut BufWriter<std::fs::File>,
    buf: &mut [u8],
    mut count: u64,
) -> Result<()> {
    while count > 0 {
        let to_read = (count as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read]).context("read")?;
        if n == 0 {
            anyhow::bail!("Unexpected EOF: {} bytes remaining to copy", count);
        }
        writer.write_all(&buf[..n]).context("write")?;
        count -= n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rollup functions (algorithm-agnostic via FlexHasher)
// ---------------------------------------------------------------------------

fn compute_parent_checksum<'a>(
    mut hasher: FlexHasher,
    items: impl Iterator<Item = (usize, &'a str)>,
) -> String {
    let mut sorted: Vec<_> = items.collect();
    sorted.sort_by_key(|(order, _)| *order);

    for (_order, checksum) in &sorted {
        hasher.update(checksum.as_bytes());
    }
    hasher.finalize_hex()
}

fn compute_addon_checksum(
    mut hasher: FlexHasher,
    files: &[ModFile],
    checksum_field: fn(&ModFile) -> &str,
) -> String {
    let mut sorted: Vec<&ModFile> = files.iter().collect();
    sorted.sort_by_cached_key(|file| clean_addon_path(&file.relative_path));

    for file in sorted {
        hasher.update(checksum_field(file).as_bytes());
        hasher.update(normalize_checksum_path(&file.relative_path).as_bytes());
    }

    hasher.finalize_hex()
}

pub(crate) fn compute_repo_checksum_for_ticks(mods: &[ProcessedMod], ticks: u64) -> String {
    let mut hasher = Sha1::new();
    hasher.update(ticks.to_string().as_bytes());

    for checksum in mods
        .iter()
        .filter(|m| m.is_required)
        .map(|m| m.checksums.unwrap_md5())
    {
        hasher.update(checksum.as_bytes());
    }

    for checksum in mods
        .iter()
        .filter(|m| !m.is_required)
        .map(|m| m.checksums.unwrap_md5())
    {
        hasher.update(checksum.as_bytes());
    }

    hex::encode(hasher.finalize()).to_uppercase()
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn clean_addon_path(path: &str) -> String {
    path.chars()
        .filter(|ch| !matches!(ch, ';' | '/' | '\\'))
        .flat_map(|ch| ch.to_uppercase())
        .collect()
}

fn normalize_checksum_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn current_utc_ticks() -> u64 {
    const TICKS_AT_UNIX_EPOCH: u64 = 621_355_968_000_000_000;
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const NANOS_PER_TICK: u32 = 100;

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    TICKS_AT_UNIX_EPOCH
        + duration.as_secs() * TICKS_PER_SECOND
        + u64::from(duration.subsec_nanos() / NANOS_PER_TICK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Checksums, FilePart, ModFile};

    #[test]
    fn addon_checksum_matches_legacy_path_sensitive_rollup() {
        let files = vec![
            ModFile {
                relative_path: "addons\\demo.pbo".to_string(),
                checksums: Checksums::Md5("AAAABBBBCCCCDDDD".to_string()),
                length: 1,
                parts: vec![FilePart {
                    path: "$$HEADER$$".to_string(),
                    checksums: Checksums::Md5("PART".to_string()),
                    start: 0,
                    length: 1,
                }],
                data_order: 1,
            },
            ModFile {
                relative_path: "Keys\\Demo.bikey".to_string(),
                checksums: Checksums::Md5("1111222233334444".to_string()),
                length: 1,
                parts: vec![],
                data_order: 0,
            },
        ];

        assert_eq!(
            compute_addon_checksum(FlexHasher::new_md5(), &files, |f| f.checksums.unwrap_md5()),
            "C9AFB66E5973B520FF6206E6C35687AC"
        );
    }

    #[test]
    fn repo_checksum_uses_sha1_ticks_then_required_then_optional() {
        let mods = vec![
            ProcessedMod {
                mod_name: "@req".to_string(),
                checksums: Checksums::Md5("REQ".to_string()),
                files: vec![],
                is_required: true,
                enabled: true,
                client_side: false,
            },
            ProcessedMod {
                mod_name: "@opt".to_string(),
                checksums: Checksums::Md5("OPT".to_string()),
                files: vec![],
                is_required: false,
                enabled: true,
                client_side: false,
            },
        ];

        assert_eq!(
            compute_repo_checksum_for_ticks(&mods, 638_780_000_000_000_000),
            "C0F49E861635B9B52C304E6813DA903C6FA689A7"
        );
    }

    #[test]
    fn blake3_addon_checksum_deterministic() {
        let files = vec![
            ModFile {
                relative_path: "addons/test.pbo".to_string(),
                checksums: Checksums::Blake3("AAAA".to_string()),
                length: 100,
                parts: vec![],
                data_order: 0,
            },
            ModFile {
                relative_path: "keys/test.bikey".to_string(),
                checksums: Checksums::Blake3("BBBB".to_string()),
                length: 50,
                parts: vec![],
                data_order: 1,
            },
        ];

        let cs1 = compute_addon_checksum(FlexHasher::new_blake3(), &files, |f| {
            f.checksums.unwrap_blake3()
        });
        let cs2 = compute_addon_checksum(FlexHasher::new_blake3(), &files, |f| {
            f.checksums.unwrap_blake3()
        });
        assert_eq!(cs1, cs2);
        assert!(!cs1.is_empty());
    }

    #[test]
    fn foxy_addon_checksum_uses_ordered_file_rollup_without_paths() {
        let files = vec![
            ModFile {
                relative_path: "addons/a.pbo".to_string(),
                checksums: Checksums::Blake3("AAAA".to_string()),
                length: 100,
                parts: vec![],
                data_order: 1,
            },
            ModFile {
                relative_path: "addons/b.pbo".to_string(),
                checksums: Checksums::Blake3("BBBB".to_string()),
                length: 50,
                parts: vec![],
                data_order: 0,
            },
        ];
        let renamed_files = vec![
            ModFile {
                relative_path: "addons/renamed-a.pbo".to_string(),
                checksums: Checksums::Blake3("AAAA".to_string()),
                length: 100,
                parts: vec![],
                data_order: 1,
            },
            ModFile {
                relative_path: "addons/renamed-b.pbo".to_string(),
                checksums: Checksums::Blake3("BBBB".to_string()),
                length: 50,
                parts: vec![],
                data_order: 0,
            },
        ];

        assert_eq!(
            compute_mod_checksums(&files, GenerationMode::Foxy).unwrap_blake3(),
            compute_mod_checksums(&renamed_files, GenerationMode::Foxy).unwrap_blake3()
        );
    }

    #[test]
    fn foxy_repo_checksum_deterministic() {
        let mods = vec![
            ProcessedMod {
                mod_name: "@a".to_string(),
                checksums: Checksums::Blake3("FOXYA".to_string()),
                files: vec![],
                is_required: true,
                enabled: true,
                client_side: false,
            },
            ProcessedMod {
                mod_name: "@b".to_string(),
                checksums: Checksums::Blake3("FOXYB".to_string()),
                files: vec![],
                is_required: false,
                enabled: true,
                client_side: false,
            },
        ];

        let cs1 = compute_foxy_repo_checksum(&mods);
        let cs2 = compute_foxy_repo_checksum(&mods);
        assert_eq!(cs1, cs2);
        assert!(!cs1.is_empty());
    }

    // ── FlexHasher ──────────────────────────────────────────────────────

    #[test]
    fn flex_hasher_md5_known_value() {
        // MD5("") = D41D8CD98F00B204E9800998ECF8427E
        let h = FlexHasher::new_md5();
        assert_eq!(h.finalize_hex(), "D41D8CD98F00B204E9800998ECF8427E");
    }

    #[test]
    fn flex_hasher_blake3_known_value() {
        // BLAKE3("") full 64-char hex
        let h = FlexHasher::new_blake3();
        let hex = h.finalize_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex, hex.to_uppercase());
    }

    #[test]
    fn flex_hasher_md5_hello() {
        let mut h = FlexHasher::new_md5();
        h.update(b"hello");
        assert_eq!(h.finalize_hex(), "5D41402ABC4B2A76B9719D911017C592");
    }

    // ── clean_addon_path ────────────────────────────────────────────────

    #[test]
    fn clean_addon_path_strips_separators_and_uppercases() {
        assert_eq!(clean_addon_path("addons/demo.pbo"), "ADDONSDEMO.PBO");
    }

    #[test]
    fn clean_addon_path_strips_backslashes() {
        assert_eq!(clean_addon_path("addons\\demo.pbo"), "ADDONSDEMO.PBO");
    }

    #[test]
    fn clean_addon_path_strips_semicolons() {
        assert_eq!(clean_addon_path("a;b;c"), "ABC");
    }

    #[test]
    fn clean_addon_path_empty() {
        assert_eq!(clean_addon_path(""), "");
    }

    // ── normalize_checksum_path ─────────────────────────────────────────

    #[test]
    fn normalize_checksum_path_backslash_to_forward_preserves_case() {
        assert_eq!(
            normalize_checksum_path("Addons\\Demo.PBO"),
            "Addons/Demo.PBO"
        );
    }

    #[test]
    fn normalize_checksum_path_already_normalized() {
        assert_eq!(
            normalize_checksum_path("addons/demo.pbo"),
            "addons/demo.pbo"
        );
    }

    #[test]
    fn normalize_checksum_path_empty() {
        assert_eq!(normalize_checksum_path(""), "");
    }

    // ── assemble_checksums ──────────────────────────────────────────────

    #[test]
    fn assemble_checksums_swifty() {
        let cs = assemble_checksums(Some("MD5HASH".to_string()), None, GenerationMode::Swifty);
        assert_eq!(cs.unwrap_md5(), "MD5HASH");
    }

    #[test]
    fn assemble_checksums_foxy() {
        let cs = assemble_checksums(None, Some("B3HASH".to_string()), GenerationMode::Foxy);
        assert_eq!(cs.unwrap_blake3(), "B3HASH");
    }

    #[test]
    fn assemble_checksums_hybrid() {
        let cs = assemble_checksums(
            Some("MD5".to_string()),
            Some("B3".to_string()),
            GenerationMode::Hybrid,
        );
        assert_eq!(cs.unwrap_md5(), "MD5");
        assert_eq!(cs.unwrap_blake3(), "B3");
    }

    // ── buf_size_for_mode ───────────────────────────────────────────────

    #[test]
    fn buf_size_swifty_is_256k() {
        assert_eq!(buf_size_for_mode(GenerationMode::Swifty), 256 * 1024);
    }

    #[test]
    fn buf_size_foxy_is_1m() {
        assert_eq!(buf_size_for_mode(GenerationMode::Foxy), 1024 * 1024);
    }

    #[test]
    fn buf_size_hybrid_is_1m() {
        assert_eq!(buf_size_for_mode(GenerationMode::Hybrid), 1024 * 1024);
    }

    // ── compute_parent_checksum ─────────────────────────────────────────

    #[test]
    fn compute_parent_checksum_deterministic() {
        let items = vec![(0usize, "AAAA"), (1, "BBBB")];
        let c1 = compute_parent_checksum(FlexHasher::new_md5(), items.clone().into_iter());
        let c2 = compute_parent_checksum(FlexHasher::new_md5(), items.into_iter());
        assert_eq!(c1, c2);
        assert!(!c1.is_empty());
    }

    #[test]
    fn compute_parent_checksum_order_matters() {
        let a = compute_parent_checksum(
            FlexHasher::new_md5(),
            vec![(0usize, "AAAA"), (1, "BBBB")].into_iter(),
        );
        let b = compute_parent_checksum(
            FlexHasher::new_md5(),
            vec![(0usize, "BBBB"), (1, "AAAA")].into_iter(),
        );
        assert_ne!(a, b);
    }

    // ── hash_file_sha1 ─────────────────────────────────────────────────

    #[test]
    fn hash_file_sha1_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello").unwrap();
        let hash = hash_file_sha1(&file).unwrap();
        // SHA-1("hello") = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
        assert_eq!(hash, "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D");
    }

    #[test]
    fn hash_file_sha1_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::File::create(&file).unwrap();
        let hash = hash_file_sha1(&file).unwrap();
        // SHA-1("") = DA39A3EE5E6B4B0D3255BFEF95601890AFD80709
        assert_eq!(hash, "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
    }

    #[test]
    fn hash_file_sha1_missing_file_errors() {
        let result = hash_file_sha1(std::path::Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }

    // ── current_utc_ticks ───────────────────────────────────────────────

    #[test]
    fn current_utc_ticks_is_reasonable() {
        let ticks = current_utc_ticks();
        // Should be well past the .NET epoch and well past Unix epoch in ticks
        let min_2024_ticks: u64 = 638_400_000_000_000_000;
        assert!(
            ticks > min_2024_ticks,
            "ticks {} should be after 2024",
            ticks
        );
    }

    // ── compute_foxy_repo_checksum ──────────────────────────────────────

    #[test]
    fn foxy_repo_checksum_separates_required_optional() {
        let mods_ab = vec![
            ProcessedMod {
                mod_name: "@a".to_string(),
                checksums: Checksums::Blake3("AAA".to_string()),
                files: vec![],
                is_required: true,
                enabled: true,
                client_side: false,
            },
            ProcessedMod {
                mod_name: "@b".to_string(),
                checksums: Checksums::Blake3("BBB".to_string()),
                files: vec![],
                is_required: false,
                enabled: true,
                client_side: false,
            },
        ];
        let mods_ba = vec![
            ProcessedMod {
                mod_name: "@a".to_string(),
                checksums: Checksums::Blake3("AAA".to_string()),
                files: vec![],
                is_required: false,
                enabled: true,
                client_side: false,
            },
            ProcessedMod {
                mod_name: "@b".to_string(),
                checksums: Checksums::Blake3("BBB".to_string()),
                files: vec![],
                is_required: true,
                enabled: true,
                client_side: false,
            },
        ];
        // Swapping required/optional should change the checksum
        assert_ne!(
            compute_foxy_repo_checksum(&mods_ab),
            compute_foxy_repo_checksum(&mods_ba)
        );
    }
}
