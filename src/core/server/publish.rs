#![cfg_attr(not(test), allow(dead_code))]

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectedFile {
    pub name: String,
    pub body: String,
}

pub(crate) fn project_player_content(files: &[ProjectedFile]) -> Vec<ProjectedFile> {
    files
        .iter()
        .filter(|file| !contains_secret(&file.name) && !contains_secret(&file.body))
        .cloned()
        .collect()
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("password")
        || lower.contains("rcon")
        || lower.contains("token")
        || lower.contains("bikey")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_omits_server_secrets() {
        let projected = project_player_content(&[
            ProjectedFile {
                name: "addons.txt".into(),
                body: "@mod_01\n".into(),
            },
            ProjectedFile {
                name: "server.cfg".into(),
                body: "password = \"secret\";\n".into(),
            },
            ProjectedFile {
                name: "server.bikey".into(),
                body: "key".into(),
            },
        ]);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].name, "addons.txt");
    }
}
