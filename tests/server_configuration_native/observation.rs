use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Expected<'a> {
    pub identity: &'a str,
    pub hostname: &'a str,
    pub crosshair: u8,
    pub third_person: u8,
    pub tactical_ping: u8,
    pub parameter: u64,
    pub profile: &'a str,
    pub mission: &'a str,
}

pub(super) struct Observed {
    pub aiming: f64,
    pub courage: f64,
}

pub(super) struct Snapshot(HashMap<PathBuf, u64>);

const ERROR_MARKERS: &[&str] = &[
    "dedicated.cfg, line",
    "network.cfg, line",
    "Managed.Arma3Profile, line",
];

/// Records the size of every engine report and owned console log before a run, so only text
/// written by the new run is inspected.
pub(super) fn snapshot(state: &Path) -> io::Result<Snapshot> {
    let mut sizes = HashMap::new();
    for (path, size) in reports(state)? {
        sizes.insert(path, size);
    }
    Ok(Snapshot(sizes))
}

pub(super) fn wait(
    state: &Path,
    before: &Snapshot,
    expected: &Expected<'_>,
    timeout: Duration,
) -> io::Result<Observed> {
    let marker = format!("FOXY_CONFIGURATION|{}|", expected.identity);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for (path, size) in reports(state)? {
            if size > 16 * 1024 * 1024 {
                return Err(io::Error::other("Engine report exceeded size bound"));
            }
            let bytes = fs::read(&path)?;
            let start = before
                .0
                .get(&path)
                .copied()
                .filter(|old| *old <= bytes.len() as u64)
                .unwrap_or(0) as usize;
            let text = String::from_utf8_lossy(&bytes[start..]);
            if let Some(error) = text
                .lines()
                .find(|line| ERROR_MARKERS.iter().any(|marker| line.contains(marker)))
            {
                return Err(io::Error::other(format!(
                    "Engine reported a generated configuration error: {error}"
                )));
            }
            if let Some(record) = text.rsplit_once(&marker).map(|(_, rest)| rest) {
                let record = record.lines().next().unwrap_or_default();
                return parse(record, expected);
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(io::Error::other(format!(
        "Fresh native output did not contain the configuration observation for {}",
        expected.identity
    )))
}

fn parse(record: &str, expected: &Expected<'_>) -> io::Result<Observed> {
    let record = record.trim().trim_end_matches('"');
    let fields = record.split('|').collect::<Vec<_>>();
    if fields.len() != 10 || fields[9] != "end" {
        return Err(io::Error::other(format!(
            "Malformed native configuration observation: {record}"
        )));
    }
    let hostname = fields[0]
        .trim_matches(['[', ']'])
        .split(',')
        .filter(|code| !code.is_empty())
        .map(|code| {
            code.trim()
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| io::Error::other(format!("Bad hostname code point in {record}")))
        })
        .collect::<io::Result<String>>()?;
    let integer = |index: usize| -> io::Result<f64> {
        fields[index]
            .trim()
            .parse::<f64>()
            .map_err(|_| io::Error::other(format!("Field {index} is not numeric in {record}")))
    };
    let matches = hostname == expected.hostname
        && integer(1)? == f64::from(expected.crosshair)
        && integer(2)? == f64::from(expected.third_person)
        && integer(3)? == f64::from(expected.tactical_ping)
        && integer(4)? == expected.parameter as f64
        && fields[7] == expected.profile
        && fields[8] == expected.mission;
    if !matches {
        return Err(io::Error::other(format!(
            "Native engine configuration differed from the committed generation. Expected host {:?}, crosshair {}, third person {}, tactical ping {}, parameter {}, profile {}, mission {}; observed host {hostname:?} and {record}",
            expected.hostname,
            expected.crosshair,
            expected.third_person,
            expected.tactical_ping,
            expected.parameter,
            expected.profile,
            expected.mission
        )));
    }
    eprintln!(
        "Native configuration observation for {}: {record}",
        expected.identity
    );
    Ok(Observed {
        aiming: integer(5)?,
        courage: integer(6)?,
    })
}

fn reports(state: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    let mut stack = vec![(state.to_path_buf(), 0usize)];
    let mut count = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if depth > 16 {
            return Err(io::Error::other("Runtime output depth exceeded bound"));
        }
        for entry in fs::read_dir(&directory)? {
            count += 1;
            if count > 8192 {
                return Err(io::Error::other("Runtime output inventory exceeded bound"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if is_link(&metadata) {
                return Err(io::Error::other("Runtime output contains a link"));
            }
            let path = entry.path();
            if metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if metadata.is_file()
                && path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("rpt") || extension.eq_ignore_ascii_case("log")
                })
            {
                found.push((path, metadata.len()));
            }
        }
    }
    Ok(found)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[test]
fn observation_parser_decodes_code_points_and_rejects_drift() {
    let expected = Expected {
        identity: "ConfigA_1",
        hostname: "A \"q\" C:\\x",
        crosshair: 1,
        third_person: 0,
        tactical_ping: 3,
        parameter: 17,
        profile: "missing",
        mission: "missing",
    };
    let codes = expected
        .hostname
        .chars()
        .map(|character| (character as u32).to_string())
        .collect::<Vec<_>>()
        .join(",");
    let record = format!("[{codes}]|1|0|3|17|0.42|0.5|missing|missing|end\"");
    let observed = parse(&record, &expected).unwrap();
    assert!((observed.aiming - 0.42).abs() < 1e-9);
    assert!(parse(&record.replace("|17|", "|23|"), &expected).is_err());
    assert!(parse(&record.replace("|1|0|3|", "|1|1|3|"), &expected).is_err());
    assert!(parse("[65]|1|0|3|17|0.4|0.5|missing|missing|end", &expected).is_err());
}
