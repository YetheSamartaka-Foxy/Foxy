use super::ContentStageControl;
use super::payload::{copy_payload, inspect_payload};
use sha1::Digest;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use tempfile::tempdir;

fn pbo(path: &Path, member: &[u8]) {
    let mut file = File::create(path).unwrap();
    file.write_all(b"\0sreV").unwrap();
    file.write_all(&[0; 16]).unwrap();
    file.write_all(&[0]).unwrap();
    file.write_all(b"mission.sqm\0").unwrap();
    file.write_all(&0u32.to_le_bytes()).unwrap();
    file.write_all(&(member.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 8]).unwrap();
    file.write_all(&(member.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 21]).unwrap();
    file.write_all(member).unwrap();
}

fn compressed_pbo(path: &Path, packed: &[u8], expanded_bytes: u32) {
    let mut file = File::create(path).unwrap();
    file.write_all(b"\0sreV").unwrap();
    file.write_all(&[0; 16]).unwrap();
    file.write_all(&[0]).unwrap();
    file.write_all(b"mission.sqm\0").unwrap();
    file.write_all(&1u32.to_le_bytes()).unwrap();
    file.write_all(&expanded_bytes.to_le_bytes()).unwrap();
    file.write_all(&[0; 8]).unwrap();
    file.write_all(&(packed.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 21]).unwrap();
    file.write_all(packed).unwrap();
}

#[test]
fn folder_inspection_and_copy_produce_reviewed_pbo() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("Foo.World");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("mission.sqm"),
        "briefingName=\"A literal mission\";",
    )
    .unwrap();
    let control = ContentStageControl::default();
    let reviewed = inspect_payload(&source, &control).unwrap();
    assert_eq!(reviewed.template, "Foo.World");
    assert_eq!(reviewed.world, "World");
    assert_eq!(
        reviewed.metadata.display_name.as_deref(),
        Some("A literal mission")
    );
    let destination = dir.path().join("Foo.World.pbo");
    copy_payload(&source, &reviewed, &destination, &control).unwrap();
    let artifact = fs::read(&destination).unwrap();
    assert_eq!(reviewed.payload_bytes, artifact.len() as u64);
    assert_eq!(
        reviewed.payload_blake3,
        blake3::hash(&artifact).to_hex().to_string()
    );
    assert_eq!(artifact[artifact.len() - 21], 0);
    assert_eq!(
        &artifact[artifact.len() - 20..],
        sha1::Sha1::digest(&artifact[..artifact.len() - 21]).as_slice()
    );
    assert_eq!(
        inspect_payload(&destination, &control).unwrap().files,
        reviewed.files
    );
    assert!(copy_payload(&source, &reviewed, &destination, &control).is_err());
}

#[test]
fn pbo_inspection_rejects_truncation_and_path_traversal() {
    let dir = tempdir().unwrap();
    let control = ContentStageControl::default();
    let good = dir.path().join("Foo.World.pbo");
    pbo(&good, b"sqm");
    let good_payload = inspect_payload(&good, &control).unwrap();
    let good_bytes = fs::read(&good).unwrap();
    assert_eq!(good_payload.payload_bytes, good_bytes.len() as u64);
    assert_eq!(
        good_payload.payload_blake3,
        blake3::hash(&good_bytes).to_hex().to_string()
    );
    let copied_dir = dir.path().join("copied");
    fs::create_dir(&copied_dir).unwrap();
    let exact_copy = copied_dir.join("Foo.World.pbo");
    copy_payload(&good, &good_payload, &exact_copy, &control).unwrap();
    assert_eq!(fs::read(&exact_copy).unwrap(), good_bytes);
    let bad = dir.path().join("Bad.World.pbo");
    pbo(&bad, b"sqm");
    let mut bytes = fs::read(&bad).unwrap();
    bytes.pop();
    fs::write(&bad, bytes).unwrap();
    assert!(inspect_payload(&bad, &control).is_err());
    let unsafe_folder = dir.path().join("Bad.World!");
    fs::create_dir(&unsafe_folder).unwrap();
    fs::write(unsafe_folder.join("mission.sqm"), "").unwrap();
    assert!(inspect_payload(&unsafe_folder, &control).is_err());

    let compressed = dir.path().join("Packed.World.pbo");
    compressed_pbo(&compressed, b"packed", 4096);
    let packed = inspect_payload(&compressed, &control).unwrap();
    assert!(
        packed
            .metadata
            .warnings
            .iter()
            .any(|warning| warning.contains("compressed"))
    );
    let packed_copy_dir = dir.path().join("packed-copy");
    fs::create_dir(&packed_copy_dir).unwrap();
    let compressed_copy = packed_copy_dir.join("Packed.World.pbo");
    copy_payload(&compressed, &packed, &compressed_copy, &control).unwrap();
    assert_eq!(
        fs::read(&compressed_copy).unwrap(),
        fs::read(&compressed).unwrap()
    );
}

#[test]
fn metadata_reads_nested_scenario_and_literal_parameter_choices_with_locations() {
    let source = concat!(
        "class Mission {\n",
        "  class ScenarioData {\n",
        "    briefingName = \"Nested title\";\n",
        "    author = \"Author\";\n",
        "    addons[] = {\"A3\", \"A3_Weapons_F\"};\n",
        "  };\n",
        "};\n",
    );
    let description = concat!(
        "onLoadName = \"Shown title\";\n",
        "class Params {\n",
        "  class Mode {\n",
        "    title = \"Mode\";\n",
        "    values[] = {0, 1};\n",
        "    texts[] = {\"Easy\", \"Hard\"};\n",
        "  };\n",
        "};\n",
    );
    let metadata = super::metadata::parse_metadata(&[
        ("mission.sqm".into(), source.as_bytes().to_vec()),
        ("description.ext".into(), description.as_bytes().to_vec()),
    ]);
    assert_eq!(metadata.display_name.as_deref(), Some("Shown title"));
    assert_eq!(metadata.author.as_deref(), Some("Author"));
    assert_eq!(metadata.declared_addons, ["A3", "A3_Weapons_F"]);
    assert_eq!(metadata.params.len(), 1);
    assert_eq!(metadata.params[0].choices, ["0: Easy", "1: Hard"]);
    assert_eq!(metadata.params[0].source, "description.ext");
    assert!(metadata.locations.iter().all(|location| location.line > 0));
}

#[test]
fn metadata_reports_binary_and_macro_values_without_guessing() {
    let metadata = super::metadata::parse_metadata(&[
        ("mission.sqm".into(), vec![0xff, 0x00]),
        (
            "description.ext".into(),
            b"#include \"other.ext\"\nonLoadName = DISPLAY_NAME;".to_vec(),
        ),
    ]);
    assert!(metadata.display_name.is_none());
    assert!(
        metadata
            .warnings
            .iter()
            .any(|warning| warning.contains("mission.sqm") && warning.contains("UTF-8"))
    );
    assert!(
        metadata
            .warnings
            .iter()
            .any(|warning| warning.contains("preprocessor or opaque values"))
    );
    assert!(
        metadata
            .warnings
            .iter()
            .any(|warning| warning.contains("display name is not a literal string"))
    );
}
