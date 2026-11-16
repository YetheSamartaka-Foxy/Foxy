use super::*;

pub(super) fn install(
    input: &Path,
    payload: &Path,
    run: &std::ffi::OsStr,
    name: &str,
    shared: bool,
) -> io::Result<()> {
    let destination = if shared {
        payload
            .join("private-instances")
            .join(run)
            .join(name)
            .join("keys")
    } else {
        payload.join("keys")
    };
    ordinary(&destination, true)?;
    let filename = match name {
        "alpha" => "FoxyNativeAlpha.bikey",
        "beta" => "FoxyNativeBeta.bikey",
        _ => return Err(io::Error::other("Unknown native key fixture")),
    };
    let source = input.join(filename);
    let length = ordinary(&source, false)?.len();
    if length == 0 || length > 16 * 1024 {
        return Err(io::Error::other("Native public-key fixture exceeds bounds"));
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.join(filename))?;
    let copied = io::copy(&mut fs::File::open(source)?.take(length + 1), &mut output)?;
    output.sync_all()?;
    if copied != length {
        return Err(io::Error::other("Native public-key fixture changed"));
    }
    Ok(())
}

pub(super) fn isolated(signatures: &[String], name: &str) -> bool {
    let (own, other) = match name {
        "alpha" => ("FoxyNativeAlpha", "FoxyNativeBeta"),
        "beta" => ("FoxyNativeBeta", "FoxyNativeAlpha"),
        _ => return false,
    };
    let contains = |key: &str| {
        signatures
            .iter()
            .any(|signature| signature == key || signature.strip_suffix(".bikey") == Some(key))
    };
    contains(own) && !contains(other) && contains("a3")
}

#[test]
fn only_the_own_fixture_key_and_retained_base_key_establish_isolation() {
    let signatures = vec!["a3".into(), "FoxyNativeAlpha".into()];
    assert!(isolated(&signatures, "alpha"));
    assert!(!isolated(&signatures, "beta"));
    assert!(!isolated(&["FoxyNativeAlpha".into()], "alpha"));
    assert!(!isolated(
        &[
            "a3".into(),
            "FoxyNativeAlpha".into(),
            "FoxyNativeBeta".into()
        ],
        "alpha"
    ));
    assert!(!isolated(
        &["a3".into(), "FoxyNativeAlpha_extra".into()],
        "alpha"
    ));
}
