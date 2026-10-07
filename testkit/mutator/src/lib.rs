use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use foxy_formats::{ContentFormat, FilePart, PboFormat};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const JOURNAL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MutationProfile {
    SingleEntry,
    Scattered,
    /// One contiguous run of entries per file, so the patch plan can coalesce
    /// its ranges; the locality counterpart of `Scattered`.
    Adjacent,
    TailTruncate,
    HeaderCorrupt,
    WholeFile,
    Delete,
    TouchOnly,
}

#[derive(Clone, Debug)]
pub struct MutateOptions {
    pub root: PathBuf,
    pub targets: Vec<PathBuf>,
    pub profile: MutationProfile,
    pub seed: u64,
    pub entry_count: usize,
    pub file_count: usize,
    pub truncate_by: u64,
    /// Put every mutated file's modification time back afterwards, so a
    /// size-and-mtime fingerprint cannot see the change (a silently changed
    /// patch source).
    pub preserve_mtime: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationJournal {
    pub version: u32,
    pub profile: MutationProfile,
    pub seed: u64,
    pub mutations: Vec<MutationRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationRecord {
    pub path: PathBuf,
    pub part_index: usize,
    pub byte_offset: u64,
    pub original_bytes: Vec<u8>,
    pub new_bytes: Vec<u8>,
    pub seed: u64,
    pub kind: MutationKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationKind {
    Replace,
    Truncate,
    Delete,
    TouchOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct MutationSummary {
    pub seed: u64,
    pub mutated_parts: usize,
    pub mutated_bytes: u64,
}

impl MutationJournal {
    pub fn summary(&self) -> MutationSummary {
        MutationSummary {
            seed: self.seed,
            mutated_parts: self.mutations.len(),
            mutated_bytes: self
                .mutations
                .iter()
                .map(|mutation| match mutation.kind {
                    MutationKind::TouchOnly => 0,
                    MutationKind::Truncate | MutationKind::Delete => {
                        mutation.original_bytes.len() as u64
                    }
                    MutationKind::Replace => mutation.new_bytes.len() as u64,
                })
                .sum(),
        }
    }
}

pub fn mutate(options: &MutateOptions, journal_path: &Path) -> Result<MutationJournal> {
    ensure!(
        !journal_path.exists(),
        "mutation journal already exists: {}",
        journal_path.display()
    );
    let journal = plan_mutations(options)?;
    write_journal(journal_path, &journal)?;
    let stamps = if options.preserve_mtime {
        modification_times(&options.root, &journal)?
    } else {
        Vec::new()
    };
    apply_mutations(&options.root, &journal)?;
    for (path, modified) in stamps {
        File::options()
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {} to restore its mtime", path.display()))?
            .set_modified(modified)?;
    }
    Ok(journal)
}

pub fn restore(root: &Path, journal_path: &Path) -> Result<MutationJournal> {
    let bytes = fs::read(journal_path)
        .with_context(|| format!("failed to read journal {}", journal_path.display()))?;
    let journal: MutationJournal = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse journal {}", journal_path.display()))?;
    ensure!(
        journal.version == JOURNAL_VERSION,
        "unsupported mutation journal version {}",
        journal.version
    );
    restore_mutations(root, &journal)?;
    Ok(journal)
}

pub fn plan_mutations(options: &MutateOptions) -> Result<MutationJournal> {
    ensure!(options.root.is_dir(), "mutation root is not a directory");
    let candidates = candidate_pbos(&options.root, &options.targets)?;
    ensure!(!candidates.is_empty(), "no PBO files found to mutate");
    let mut rng = DeterministicRng::new(options.seed);

    let mutations = match options.profile {
        MutationProfile::SingleEntry => plan_single_entry(&candidates, options.seed, &mut rng)?,
        MutationProfile::Scattered => plan_scattered(
            &candidates,
            options.entry_count,
            options.file_count,
            options.seed,
            &mut rng,
        )?,
        MutationProfile::Adjacent => plan_adjacent(
            &candidates,
            options.entry_count,
            options.file_count,
            options.seed,
            &mut rng,
        )?,
        MutationProfile::TailTruncate => {
            plan_tail_truncate(&candidates, options.truncate_by, options.seed, &mut rng)?
        }
        MutationProfile::HeaderCorrupt => plan_header_corrupt(&candidates, options.seed, &mut rng)?,
        MutationProfile::WholeFile => plan_whole_file(&candidates, options.seed, &mut rng)?,
        MutationProfile::Delete => plan_delete(&candidates, options.seed, &mut rng)?,
        MutationProfile::TouchOnly => plan_touch_only(&candidates, options.seed, &mut rng)?,
    };

    Ok(MutationJournal {
        version: JOURNAL_VERSION,
        profile: options.profile,
        seed: options.seed,
        mutations,
    })
}

fn write_journal(path: &Path, journal: &MutationJournal) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create journal directory {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create journal {}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, journal)
        .with_context(|| format!("failed to write journal {}", path.display()))?;
    file.write_all(b"\n")?;
    file.sync_all()
        .with_context(|| format!("failed to persist journal {}", path.display()))?;
    Ok(())
}

fn apply_mutations(root: &Path, journal: &MutationJournal) -> Result<()> {
    for mutation in &journal.mutations {
        let path = resolve_journal_path(root, &mutation.path)?;
        match mutation.kind {
            MutationKind::Replace => write_at(&path, mutation.byte_offset, &mutation.new_bytes)?,
            MutationKind::Truncate => File::options()
                .write(true)
                .open(&path)
                .with_context(|| format!("failed to open {} for truncation", path.display()))?
                .set_len(mutation.byte_offset)
                .with_context(|| format!("failed to truncate {}", path.display()))?,
            MutationKind::Delete => fs::remove_file(&path)
                .with_context(|| format!("failed to delete {}", path.display()))?,
            MutationKind::TouchOnly => {
                let metadata = fs::metadata(&path)
                    .with_context(|| format!("failed to inspect {} for touch", path.display()))?;
                let bytes = fs::read(&path)
                    .with_context(|| format!("failed to read {} for touch", path.display()))?;
                fs::write(&path, bytes)
                    .with_context(|| format!("failed to rewrite {} for touch", path.display()))?;
                let modified = metadata.modified().with_context(|| {
                    format!("failed to read modified time for {}", path.display())
                })?;
                File::options()
                    .write(true)
                    .open(&path)?
                    .set_modified(modified + Duration::from_secs(1))?;
            }
        }
    }
    Ok(())
}

fn restore_mutations(root: &Path, journal: &MutationJournal) -> Result<()> {
    for mutation in journal.mutations.iter().rev() {
        let path = resolve_journal_path(root, &mutation.path)?;
        match mutation.kind {
            MutationKind::Replace | MutationKind::Truncate => {
                write_at(&path, mutation.byte_offset, &mutation.original_bytes)?;
            }
            MutationKind::Delete => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, &mutation.original_bytes)
                    .with_context(|| format!("failed to restore {}", path.display()))?;
            }
            MutationKind::TouchOnly => {}
        }
    }
    Ok(())
}

fn write_at(path: &Path, offset: u64, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {} for mutation", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write mutation to {}", path.display()))?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone)]
struct PboCandidate {
    relative_path: PathBuf,
    absolute_path: PathBuf,
    parts: Vec<FilePart>,
}

fn candidate_pbos(root: &Path, targets: &[PathBuf]) -> Result<Vec<PboCandidate>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve mutation root {}", root.display()))?;
    let mut paths = if targets.is_empty() {
        let mut paths = Vec::new();
        collect_pbos(&root, &mut paths)?;
        paths
    } else {
        targets
            .iter()
            .map(|target| resolve_target(&root, target))
            .collect::<Result<Vec<_>>>()?
    };
    paths.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    paths.dedup();

    paths
        .into_iter()
        .map(|absolute_path| {
            ensure!(
                absolute_path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pbo")),
                "mutation target is not a PBO: {}",
                absolute_path.display()
            );
            let relative_path = absolute_path
                .strip_prefix(&root)
                .context("mutation target escaped its root")?
                .to_path_buf();
            let parts = PboFormat
                .parse_parts(&absolute_path)
                .with_context(|| format!("failed to parse {}", absolute_path.display()))?;
            Ok(PboCandidate {
                relative_path,
                absolute_path,
                parts,
            })
        })
        .collect()
}

fn collect_pbos(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to enumerate {}", directory.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_pbos(&entry.path(), paths)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pbo"))
        {
            paths.push(entry.path());
        }
    }
    Ok(())
}

fn resolve_target(root: &Path, target: &Path) -> Result<PathBuf> {
    let path = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve mutation target {}", path.display()))?;
    ensure!(path.starts_with(root), "mutation target escaped its root");
    ensure!(path.is_file(), "mutation target is not a file");
    Ok(path)
}

fn resolve_journal_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "journal contains an unsafe path"
    );
    Ok(root.join(relative))
}

fn entry_parts(candidate: &PboCandidate) -> impl Iterator<Item = (usize, &FilePart)> {
    candidate
        .parts
        .iter()
        .enumerate()
        .skip(1)
        .take(candidate.parts.len().saturating_sub(2))
        .filter(|(_, part)| part.length > 0)
}

fn plan_single_entry(
    candidates: &[PboCandidate],
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    let mut entries = all_entries(candidates);
    ensure!(!entries.is_empty(), "no non-empty PBO entries found");
    shuffle(&mut entries, rng);
    Ok(vec![flip_entry(&entries[0], seed, rng)?])
}

fn plan_scattered(
    candidates: &[PboCandidate],
    entry_count: usize,
    file_count: usize,
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    ensure!(entry_count > 0, "scattered entry count must be positive");
    ensure!(file_count > 0, "scattered file count must be positive");
    ensure!(
        entry_count >= file_count,
        "scattered entry count must be at least the file count"
    );
    let mut eligible_files: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| entry_parts(candidate).next().is_some())
        .map(|(index, _)| index)
        .collect();
    ensure!(
        eligible_files.len() >= file_count,
        "requested {file_count} files but only {} contain entries",
        eligible_files.len()
    );
    shuffle(&mut eligible_files, rng);
    eligible_files.truncate(file_count);

    let mut selected = Vec::with_capacity(entry_count);
    let mut remaining = Vec::new();
    for file_index in eligible_files {
        let mut entries: Vec<EntryCandidate<'_>> = entry_parts(&candidates[file_index])
            .map(|(part_index, part)| EntryCandidate {
                candidate: &candidates[file_index],
                part_index,
                part,
            })
            .collect();
        shuffle(&mut entries, rng);
        selected.push(entries.remove(0));
        remaining.extend(entries);
    }
    shuffle(&mut remaining, rng);
    ensure!(
        selected.len() + remaining.len() >= entry_count,
        "requested {entry_count} entries but selected files contain only {}",
        selected.len() + remaining.len()
    );
    selected.extend(remaining.into_iter().take(entry_count - selected.len()));
    selected
        .iter()
        .map(|entry| flip_entry(entry, seed, rng))
        .collect()
}

fn modification_times(
    root: &Path,
    journal: &MutationJournal,
) -> Result<Vec<(PathBuf, std::time::SystemTime)>> {
    let mut stamps: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for mutation in &journal.mutations {
        let path = resolve_journal_path(root, &mutation.path)?;
        if stamps.iter().any(|(known, _)| *known == path) {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("failed to read modified time for {}", path.display()))?;
        stamps.push((path, modified));
    }
    Ok(stamps)
}

fn plan_adjacent(
    candidates: &[PboCandidate],
    entry_count: usize,
    file_count: usize,
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    ensure!(entry_count > 0, "adjacent entry count must be positive");
    ensure!(file_count > 0, "adjacent file count must be positive");
    ensure!(
        entry_count >= file_count,
        "adjacent entry count must be at least the file count"
    );
    let per_file = entry_count / file_count;
    let mut eligible_files: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| entry_parts(candidate).count() >= per_file)
        .map(|(index, _)| index)
        .collect();
    ensure!(
        eligible_files.len() >= file_count,
        "requested {file_count} files with {per_file} adjacent entries but only {} qualify",
        eligible_files.len()
    );
    shuffle(&mut eligible_files, rng);
    eligible_files.truncate(file_count);

    let mut mutations = Vec::with_capacity(entry_count);
    for file_index in eligible_files {
        let entries: Vec<EntryCandidate<'_>> = entry_parts(&candidates[file_index])
            .map(|(part_index, part)| EntryCandidate {
                candidate: &candidates[file_index],
                part_index,
                part,
            })
            .collect();
        let start = rng.index(entries.len() - per_file + 1);
        for entry in &entries[start..start + per_file] {
            mutations.push(flip_entry(entry, seed, rng)?);
        }
    }
    Ok(mutations)
}

fn plan_tail_truncate(
    candidates: &[PboCandidate],
    truncate_by: u64,
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    ensure!(
        truncate_by > 0,
        "tail truncation must remove at least one byte"
    );
    let mut eligible: Vec<&PboCandidate> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .parts
                .last()
                .is_some_and(|part| part.length >= truncate_by)
        })
        .collect();
    ensure!(
        !eligible.is_empty(),
        "no PBO trailer is large enough to truncate by {truncate_by} bytes"
    );
    shuffle(&mut eligible, rng);
    let candidate = eligible[0];
    let file_len = fs::metadata(&candidate.absolute_path)?.len();
    let offset = file_len - truncate_by;
    Ok(vec![MutationRecord {
        path: candidate.relative_path.clone(),
        part_index: candidate.parts.len() - 1,
        byte_offset: offset,
        original_bytes: read_span(&candidate.absolute_path, offset, truncate_by)?,
        new_bytes: Vec::new(),
        seed,
        kind: MutationKind::Truncate,
    }])
}

fn plan_header_corrupt(
    candidates: &[PboCandidate],
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    let candidate = &candidates[rng.index(candidates.len())];
    let original = read_span(&candidate.absolute_path, 0, 1)?;
    Ok(vec![MutationRecord {
        path: candidate.relative_path.clone(),
        part_index: 0,
        byte_offset: 0,
        original_bytes: original.clone(),
        new_bytes: vec![different_byte(original[0], rng)],
        seed,
        kind: MutationKind::Replace,
    }])
}

fn plan_whole_file(
    candidates: &[PboCandidate],
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    let candidate = &candidates[rng.index(candidates.len())];
    let mut records = Vec::new();
    for (part_index, part) in candidate.parts.iter().enumerate() {
        if part.length == 0 {
            continue;
        }
        let original = read_span(&candidate.absolute_path, part.start, part.length)?;
        let mut new = original.clone();
        if part_index == 0 {
            ensure!(part.length > 5, "PBO header has no safe mutation field");
            new[5] = different_byte(new[5], rng);
        } else {
            rng.fill(&mut new);
            if new == original {
                new[0] = different_byte(new[0], rng);
            }
        }
        records.push(MutationRecord {
            path: candidate.relative_path.clone(),
            part_index,
            byte_offset: part.start,
            original_bytes: original,
            new_bytes: new,
            seed,
            kind: MutationKind::Replace,
        });
    }
    Ok(records)
}

fn plan_delete(
    candidates: &[PboCandidate],
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    let candidate = &candidates[rng.index(candidates.len())];
    Ok(vec![MutationRecord {
        path: candidate.relative_path.clone(),
        part_index: 0,
        byte_offset: 0,
        original_bytes: fs::read(&candidate.absolute_path)?,
        new_bytes: Vec::new(),
        seed,
        kind: MutationKind::Delete,
    }])
}

fn plan_touch_only(
    candidates: &[PboCandidate],
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<Vec<MutationRecord>> {
    let candidate = &candidates[rng.index(candidates.len())];
    Ok(vec![MutationRecord {
        path: candidate.relative_path.clone(),
        part_index: 0,
        byte_offset: 0,
        original_bytes: Vec::new(),
        new_bytes: Vec::new(),
        seed,
        kind: MutationKind::TouchOnly,
    }])
}

#[derive(Clone)]
struct EntryCandidate<'a> {
    candidate: &'a PboCandidate,
    part_index: usize,
    part: &'a FilePart,
}

fn all_entries(candidates: &[PboCandidate]) -> Vec<EntryCandidate<'_>> {
    candidates
        .iter()
        .flat_map(|candidate| {
            entry_parts(candidate).map(|(part_index, part)| EntryCandidate {
                candidate,
                part_index,
                part,
            })
        })
        .collect()
}

fn flip_entry(
    entry: &EntryCandidate<'_>,
    seed: u64,
    rng: &mut DeterministicRng,
) -> Result<MutationRecord> {
    let offset = entry.part.start + rng.next_u64() % entry.part.length;
    let original = read_span(&entry.candidate.absolute_path, offset, 1)?;
    Ok(MutationRecord {
        path: entry.candidate.relative_path.clone(),
        part_index: entry.part_index,
        byte_offset: offset,
        original_bytes: original.clone(),
        new_bytes: vec![different_byte(original[0], rng)],
        seed,
        kind: MutationKind::Replace,
    })
}

fn read_span(path: &Path, offset: u64, length: u64) -> Result<Vec<u8>> {
    let length: usize = length.try_into().context("mutation span is too large")?;
    let mut bytes = vec![0; length];
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn different_byte(original: u8, rng: &mut DeterministicRng) -> u8 {
    original ^ ((rng.next_u64() as u8) | 1)
}

fn shuffle<T>(values: &mut [T], rng: &mut DeterministicRng) {
    for index in (1..values.len()).rev() {
        values.swap(index, rng.index(index + 1));
    }
}

struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, length: usize) -> usize {
        assert!(length > 0);
        (self.next_u64() % length as u64) as usize
    }

    fn fill(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let random = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&random[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn push_entry(bytes: &mut Vec<u8>, name: &str, length: u32) {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
    }

    fn synthetic_pbo(marker: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(0);
        bytes.extend_from_slice(b"sreV");
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.push(0);
        push_entry(&mut bytes, "first.bin", 5);
        push_entry(&mut bytes, "second.bin", 7);
        push_entry(&mut bytes, "third.bin", 3);
        push_entry(&mut bytes, "", 0);
        bytes.extend_from_slice(&[marker; 5]);
        bytes.extend_from_slice(&[marker.wrapping_add(1); 7]);
        bytes.extend_from_slice(&[marker.wrapping_add(2); 3]);
        bytes.extend_from_slice(b"trailer-data");
        bytes
    }

    fn fixture() -> (TempDir, BTreeMap<PathBuf, Vec<u8>>) {
        let temp = TempDir::new().unwrap();
        let nested = temp.path().join("addons");
        fs::create_dir(&nested).unwrap();
        let mut originals = BTreeMap::new();
        for (name, marker) in [("a.pbo", 10), ("b.pbo", 30), ("c.pbo", 50)] {
            let relative = PathBuf::from("addons").join(name);
            let bytes = synthetic_pbo(marker);
            fs::write(temp.path().join(&relative), &bytes).unwrap();
            originals.insert(relative, bytes);
        }
        (temp, originals)
    }

    fn options(root: &Path, profile: MutationProfile) -> MutateOptions {
        MutateOptions {
            root: root.to_path_buf(),
            targets: Vec::new(),
            profile,
            seed: 42,
            entry_count: 4,
            file_count: 2,
            truncate_by: 3,
            preserve_mtime: false,
        }
    }

    fn assert_restores(
        temp: &TempDir,
        originals: &BTreeMap<PathBuf, Vec<u8>>,
        journal_path: &Path,
    ) {
        restore(temp.path(), journal_path).unwrap();
        for (path, bytes) in originals {
            assert_eq!(fs::read(temp.path().join(path)).unwrap(), *bytes);
        }
    }

    #[test]
    fn single_entry_changes_one_entry_and_restores() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::SingleEntry),
            &journal_path,
        )
        .unwrap();

        assert!(journal_path.is_file());
        assert_eq!(journal.mutations.len(), 1);
        assert!(journal.mutations[0].part_index > 0);
        assert_eq!(journal.mutations[0].original_bytes.len(), 1);
        assert!(
            PboFormat
                .parse_parts(&temp.path().join(&journal.mutations[0].path))
                .is_ok()
        );
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn scattered_changes_requested_entries_across_requested_files() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::Scattered),
            &journal_path,
        )
        .unwrap();

        assert_eq!(journal.mutations.len(), 4);
        let paths: std::collections::BTreeSet<_> =
            journal.mutations.iter().map(|item| &item.path).collect();
        assert_eq!(paths.len(), 2);
        assert!(journal.mutations.iter().all(|item| item.part_index > 0));
        assert!(
            paths
                .iter()
                .all(|path| PboFormat.parse_parts(&temp.path().join(path)).is_ok())
        );
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn adjacent_changes_a_contiguous_run_per_file() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::Adjacent),
            &journal_path,
        )
        .unwrap();

        assert_eq!(journal.mutations.len(), 4);
        let mut by_path: BTreeMap<&PathBuf, Vec<usize>> = BTreeMap::new();
        for item in &journal.mutations {
            by_path.entry(&item.path).or_default().push(item.part_index);
        }
        assert_eq!(by_path.len(), 2);
        for indexes in by_path.values() {
            assert_eq!(indexes.len(), 2);
            assert_eq!(indexes[1], indexes[0] + 1, "entries must be adjacent");
        }
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn preserve_mtime_hides_the_change_from_a_timestamp_fingerprint() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let before: BTreeMap<_, _> = originals
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    fs::metadata(temp.path().join(path))
                        .unwrap()
                        .modified()
                        .unwrap(),
                )
            })
            .collect();
        std::thread::sleep(Duration::from_millis(20));
        let mut options = options(temp.path(), MutationProfile::Scattered);
        options.preserve_mtime = true;
        let journal = mutate(&options, &journal_path).unwrap();
        for item in &journal.mutations {
            let full = temp.path().join(&item.path);
            assert_ne!(fs::read(&full).unwrap(), originals[&item.path]);
            assert_eq!(
                fs::metadata(&full).unwrap().modified().unwrap(),
                before[&item.path]
            );
        }
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn tail_truncate_changes_only_the_trailer_and_restores() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::TailTruncate),
            &journal_path,
        )
        .unwrap();
        let record = &journal.mutations[0];

        assert_eq!(record.kind, MutationKind::Truncate);
        assert_eq!(record.original_bytes.len(), 3);
        assert!(
            PboFormat
                .parse_parts(&temp.path().join(&record.path))
                .is_ok()
        );
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn header_corrupt_is_a_parse_error_and_restores() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::HeaderCorrupt),
            &journal_path,
        )
        .unwrap();
        let record = &journal.mutations[0];

        assert_eq!(record.part_index, 0);
        assert_eq!(record.byte_offset, 0);
        assert!(
            PboFormat
                .parse_parts(&temp.path().join(&record.path))
                .is_err()
        );
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn whole_file_changes_every_non_empty_part_without_breaking_parse() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::WholeFile),
            &journal_path,
        )
        .unwrap();
        let path = temp.path().join(&journal.mutations[0].path);
        let parsed = PboFormat.parse_parts(&path).unwrap();

        assert_eq!(journal.mutations.len(), parsed.len());
        assert!(
            journal
                .mutations
                .iter()
                .all(|record| record.original_bytes != record.new_bytes)
        );
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn delete_removes_one_file_and_restores() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let journal = mutate(
            &options(temp.path(), MutationProfile::Delete),
            &journal_path,
        )
        .unwrap();

        assert_eq!(journal.mutations.len(), 1);
        assert!(!temp.path().join(&journal.mutations[0].path).exists());
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn touch_only_rewrites_identical_bytes_and_restores() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        let target = temp.path().join("addons/a.pbo");
        let old_modified = fs::metadata(&target).unwrap().modified().unwrap();
        let mut options = options(temp.path(), MutationProfile::TouchOnly);
        options.targets = vec![PathBuf::from("addons/a.pbo")];
        let journal = mutate(&options, &journal_path).unwrap();

        assert_eq!(journal.summary().mutated_bytes, 0);
        assert_eq!(
            fs::read(temp.path().join(&journal.mutations[0].path)).unwrap(),
            originals[&journal.mutations[0].path]
        );
        assert!(fs::metadata(&target).unwrap().modified().unwrap() > old_modified);
        assert_restores(&temp, &originals, &journal_path);
    }

    #[test]
    fn the_same_seed_produces_the_same_plan() {
        let (temp, _) = fixture();
        let first = plan_mutations(&options(temp.path(), MutationProfile::Scattered)).unwrap();
        let second = plan_mutations(&options(temp.path(), MutationProfile::Scattered)).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn an_existing_journal_prevents_any_mutation() {
        let (temp, originals) = fixture();
        let journal_path = temp.path().join("mutations.json");
        fs::write(&journal_path, b"already here").unwrap();

        assert!(
            mutate(
                &options(temp.path(), MutationProfile::SingleEntry),
                &journal_path
            )
            .is_err()
        );
        for (path, bytes) in originals {
            assert_eq!(fs::read(temp.path().join(path)).unwrap(), bytes);
        }
    }

    #[test]
    fn journal_paths_cannot_escape_the_root() {
        let (temp, _) = fixture();
        let journal = MutationJournal {
            version: JOURNAL_VERSION,
            profile: MutationProfile::Delete,
            seed: 1,
            mutations: vec![MutationRecord {
                path: PathBuf::from("..").join("outside.pbo"),
                part_index: 0,
                byte_offset: 0,
                original_bytes: vec![1],
                new_bytes: Vec::new(),
                seed: 1,
                kind: MutationKind::Delete,
            }],
        };

        assert!(apply_mutations(temp.path(), &journal).is_err());
    }

    #[test]
    fn restore_replays_overlapping_records_in_reverse() {
        let (temp, _) = fixture();
        let path = PathBuf::from("addons/a.pbo");
        let absolute = temp.path().join(&path);
        let original = fs::read(&absolute).unwrap()[0];
        let journal = MutationJournal {
            version: JOURNAL_VERSION,
            profile: MutationProfile::SingleEntry,
            seed: 1,
            mutations: vec![
                MutationRecord {
                    path: path.clone(),
                    part_index: 0,
                    byte_offset: 0,
                    original_bytes: vec![original],
                    new_bytes: vec![1],
                    seed: 1,
                    kind: MutationKind::Replace,
                },
                MutationRecord {
                    path,
                    part_index: 0,
                    byte_offset: 0,
                    original_bytes: vec![1],
                    new_bytes: vec![2],
                    seed: 1,
                    kind: MutationKind::Replace,
                },
            ],
        };

        apply_mutations(temp.path(), &journal).unwrap();
        assert_eq!(fs::read(&absolute).unwrap()[0], 2);
        restore_mutations(temp.path(), &journal).unwrap();
        assert_eq!(fs::read(&absolute).unwrap()[0], original);
    }
}
