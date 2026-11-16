use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "foxy-i18n-plural-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fs::create_dir_all(fixture.0.join("src/ui/locales")).unwrap();
        for locale in ["en", "cs"] {
            let value = if locale == "en" {
                "Files {count}"
            } else {
                "Soubory {count}"
            };
            fs::write(
                fixture.0.join(format!("src/ui/locales/{locale}.json")),
                format!("{{\n    \"Files {{count}}\": \"{value}\"\n}}\n"),
            )
            .unwrap();
        }
        fixture
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn apply(&self, value: &str) -> Output {
        let batch = serde_json::json!({"cs": {"Files {count}.few": value}});
        let path = self.0.join("batch.json");
        fs::write(&path, serde_json::to_string(&batch).unwrap()).unwrap();
        Command::new(env!("CARGO_BIN_EXE_locale-apply"))
            .args(["--translations", "batch.json", "--remove-extra"])
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    fn check(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_i18n-checker"))
            .args([
                "--strict",
                "--strict-placeholders",
                "--audit-changed-since",
                "HEAD",
            ])
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn locale_only_plurals_are_applied_preserved_and_audited() {
    let fixture = Fixture::new();
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "src"]);
    fixture.git(&[
        "-c",
        "user.name=Locale Test",
        "-c",
        "user.email=locale@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=disabled-hooks",
        "commit",
        "--quiet",
        "-m",
        "Locale fixture",
    ]);

    let applied = fixture.apply("Soubory {count}");
    assert!(applied.status.success(), "{applied:?}");
    let raw = fs::read_to_string(fixture.0.join("src/ui/locales/cs.json")).unwrap();
    assert!(raw.contains("\"Files {count}\": \"Soubory {count}\",\n    \"Files {count}.few\""));

    let applied_again = fixture.apply("Soubory {count}");
    assert!(applied_again.status.success(), "{applied_again:?}");
    let checked = fixture.check();
    assert!(checked.status.success(), "{checked:?}");
    assert!(
        String::from_utf8(checked.stdout)
            .unwrap()
            .contains("Audited 1 changed locale value(s)")
    );

    let invalid = fixture.apply("Soubory {pocet}");
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8(invalid.stdout)
            .unwrap()
            .contains("placeholder mismatch")
    );

    let untranslated = fixture.apply("Files {count}");
    assert!(untranslated.status.success(), "{untranslated:?}");
    let checked = fixture.check();
    assert!(!checked.status.success());
    assert!(
        String::from_utf8(checked.stdout)
            .unwrap()
            .contains("changed value still equals en.json")
    );
}
