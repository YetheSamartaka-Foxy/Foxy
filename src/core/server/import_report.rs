#![cfg_attr(not(test), allow(dead_code))]

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportPathReport {
    pub unresolved: Vec<String>,
}

pub(crate) fn report_unresolved_paths(document: &str) -> ImportPathReport {
    let mut unresolved = Vec::new();
    for token in document.split_whitespace() {
        let token =
            token.trim_matches(|character: char| matches!(character, '"' | '\'' | ',' | ';'));
        if looks_absolute(token) && !unresolved.iter().any(|seen| seen == token) {
            unresolved.push(token.to_string());
        }
    }
    ImportPathReport { unresolved }
}

fn looks_absolute(token: &str) -> bool {
    let bytes = token.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    token.starts_with('/') || token.starts_with("\\\\") || drive || token.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unresolved_path_is_reported_and_not_opened() {
        let missing = "C:/missing/server.cfg";
        let report = report_unresolved_paths(&format!("template = \"{missing}\";"));
        assert_eq!(report.unresolved, vec![missing.to_string()]);
        assert!(!std::path::Path::new(missing).exists());
    }
}
