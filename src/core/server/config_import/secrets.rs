use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::{ServerInstanceId, ServerStore, ServerStoreError};

const SECRET_SCHEMA: u32 = 1;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_RECORD_BYTES: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRecord {
    schema: u32,
    space_id: String,
    instance_id: String,
    reference: String,
    context_fingerprint: String,
    ciphertext: Vec<u8>,
}

impl ServerStore {
    pub(in crate::core::server) fn store_config_secret(
        &self,
        id: &ServerInstanceId,
        reference: &str,
        value: &str,
    ) -> Result<(), ServerStoreError> {
        validate_reference(reference)?;
        validate_value(value)?;
        let path = self.secret_path(id, reference);
        let root = path.parent().ok_or(ServerStoreError::UnsafePath)?;
        create_private_dirs(root)?;
        check_private_secret_dirs(root, &self.context().space_dir())?;
        let ciphertext = protect(value.as_bytes(), &self.secret_entropy(id, reference))?;
        let record = SecretRecord {
            schema: SECRET_SCHEMA,
            space_id: self.context().space_id().to_owned(),
            instance_id: id.as_str().to_owned(),
            reference: reference.to_owned(),
            context_fingerprint: self.activation_context_fingerprint(),
            ciphertext,
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| ServerStoreError::Corrupt)?;
        match read_secret_file(&path, &self.context().space_dir()) {
            Ok(existing) => {
                let decoded = self.decode_record(id, reference, &existing)?;
                if decoded == value {
                    Ok(())
                } else {
                    Err(ServerStoreError::Ownership)
                }
            }
            Err(ServerStoreError::NotFound) => {
                let mut temp =
                    tempfile::NamedTempFile::new_in(root).map_err(|_| ServerStoreError::Io)?;
                set_private_file(temp.as_file())?;
                temp.write_all(&bytes).map_err(|_| ServerStoreError::Io)?;
                temp.as_file()
                    .sync_all()
                    .map_err(|_| ServerStoreError::Io)?;
                temp.persist_noclobber(&path)
                    .map_err(|_| ServerStoreError::Ownership)?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(in crate::core::server) fn resolve_config_secret(
        &self,
        id: &ServerInstanceId,
        reference: &str,
    ) -> Result<String, ServerStoreError> {
        validate_reference(reference)?;
        let bytes = read_secret_file(
            &self.secret_path(id, reference),
            &self.context().space_dir(),
        )?;
        self.decode_record(id, reference, &bytes)
    }

    fn secret_path(&self, id: &ServerInstanceId, reference: &str) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/credentials")
            .join(id.as_str())
            .join(format!("{reference}.json"))
    }

    fn secret_entropy(&self, id: &ServerInstanceId, reference: &str) -> Vec<u8> {
        let mut bytes = b"Foxy config secret v1\0".to_vec();
        bytes.extend_from_slice(self.activation_context_fingerprint().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.context().space_id().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(id.as_str().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(reference.as_bytes());
        bytes
    }

    fn decode_record(
        &self,
        id: &ServerInstanceId,
        reference: &str,
        bytes: &[u8],
    ) -> Result<String, ServerStoreError> {
        let record: SecretRecord =
            serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if record.schema != SECRET_SCHEMA
            || record.space_id != self.context().space_id()
            || record.instance_id != id.as_str()
            || record.reference != reference
            || record.context_fingerprint != self.activation_context_fingerprint()
        {
            return Err(ServerStoreError::Ownership);
        }
        let plain = unprotect(&record.ciphertext, &self.secret_entropy(id, reference))?;
        if plain.len() > MAX_SECRET_BYTES || plain.contains(&0) {
            return Err(ServerStoreError::Corrupt);
        }
        String::from_utf8(plain).map_err(|_| ServerStoreError::Corrupt)
    }
}

fn validate_reference(reference: &str) -> Result<(), ServerStoreError> {
    let suffix = reference.strip_prefix("secret_").unwrap_or_default();
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ServerStoreError::Invalid("Invalid secret reference".into()));
    }
    Ok(())
}

fn validate_value(value: &str) -> Result<(), ServerStoreError> {
    if value.len() > MAX_SECRET_BYTES || value.as_bytes().contains(&0) {
        return Err(ServerStoreError::Invalid("Invalid secret value".into()));
    }
    Ok(())
}

fn read_secret_file(path: &Path, space_dir: &Path) -> Result<Vec<u8>, ServerStoreError> {
    let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
    super::super::store::check_directory_chain(parent)?;
    check_private_secret_dirs(parent, space_dir)?;
    #[cfg(windows)]
    open_protected_secret_dirs(space_dir, parent)?;
    #[cfg(windows)]
    super::super::store::check_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ServerStoreError::NotFound
        } else {
            ServerStoreError::UnsafePath
        }
    })?;
    check_open_file(&file)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file
            .metadata()
            .map_err(|_| ServerStoreError::Io)?
            .file_attributes()
            & 0x400
            != 0
        {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerStoreError::Io)?;
    if bytes.len() >= MAX_RECORD_BYTES {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(bytes)
}

fn check_chain(path: &Path) -> Result<(), ServerStoreError> {
    super::super::store::check_directory_chain(path)
}

fn check_private_secret_dirs(path: &Path, space_dir: &Path) -> Result<(), ServerStoreError> {
    let private_root = space_dir.join("server/credentials");
    let relative = path
        .strip_prefix(&private_root)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    let mut current = private_root;
    if current.exists() {
        let metadata = fs::symlink_metadata(&current).map_err(|_| ServerStoreError::UnsafePath)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ServerStoreError::UnsafePath);
        }
        check_private_metadata(&metadata, true)?;
    }
    for part in std::iter::once(std::ffi::OsStr::new(""))
        .chain(relative.components().map(|c| c.as_os_str()))
    {
        if !part.is_empty() {
            current.push(part);
        }
        if current.exists() {
            let metadata =
                fs::symlink_metadata(&current).map_err(|_| ServerStoreError::UnsafePath)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(ServerStoreError::UnsafePath);
            }
            check_private_metadata(&metadata, true)?;
        }
    }
    Ok(())
}

fn create_private_dirs(path: &Path) -> Result<(), ServerStoreError> {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while !current.exists() {
        missing.push(current.clone());
        if !current.pop() {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    check_chain(&current)?;
    for dir in missing.into_iter().rev() {
        #[cfg(windows)]
        {
            foxy_supervisor::control::handoff::ProtectedDirectory::create(&dir)
                .map_err(|_| ServerStoreError::UnsafePath)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut b = fs::DirBuilder::new();
            b.mode(0o700)
                .create(&dir)
                .map_err(|_| ServerStoreError::Io)?;
        }
        #[cfg(not(any(unix, windows)))]
        fs::create_dir(&dir).map_err(|_| ServerStoreError::Io)?;
        check_chain(&dir)?;
    }
    Ok(())
}

#[cfg(windows)]
fn open_protected_secret_dirs(space_dir: &Path, path: &Path) -> Result<(), ServerStoreError> {
    let root = space_dir.join("server/credentials");
    let relative = path
        .strip_prefix(&root)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    foxy_supervisor::control::handoff::ProtectedDirectory::open(&root)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    let instance_dir = root.join(relative);
    foxy_supervisor::control::handoff::ProtectedDirectory::open(&instance_dir)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    Ok(())
}

#[cfg(unix)]
fn check_private_metadata(
    metadata: &fs::Metadata,
    directory: bool,
) -> Result<(), ServerStoreError> {
    use std::os::unix::fs::MetadataExt;
    let mode = metadata.mode();
    if metadata.uid() != unsafe { libc::geteuid() }
        || mode & 0o077 != 0
        || (directory && mode & 0o700 != 0o700)
    {
        return Err(ServerStoreError::UnsafePath);
    }
    if !directory && metadata.nlink() != 1 {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

#[cfg(windows)]
fn check_private_metadata(_: &fs::Metadata, _: bool) -> Result<(), ServerStoreError> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn check_private_metadata(_: &fs::Metadata, _: bool) -> Result<(), ServerStoreError> {
    Ok(())
}

fn check_open_file(file: &File) -> Result<(), ServerStoreError> {
    let metadata = file.metadata().map_err(|_| ServerStoreError::Io)?;
    if !metadata.is_file() {
        return Err(ServerStoreError::UnsafePath);
    }
    check_private_metadata(&metadata, false)
}

fn set_private_file(_file: &File) -> Result<(), ServerStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerStoreError::Io)?;
    }
    Ok(())
}

#[cfg(windows)]
fn protect(input: &[u8], entropy: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    dpapi(input, entropy, true)
}
#[cfg(windows)]
fn unprotect(input: &[u8], entropy: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    dpapi(input, entropy, false)
}

#[cfg(windows)]
fn dpapi(input: &[u8], entropy: &[u8], encrypt: bool) -> Result<Vec<u8>, ServerStoreError> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Blob {
        size: u32,
        data: *mut u8,
    }
    #[link(name = "Crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            a: *const Blob,
            b: *const u16,
            c: *const Blob,
            d: *mut c_void,
            e: *mut c_void,
            f: u32,
            g: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            a: *const Blob,
            b: *mut *mut u16,
            c: *const Blob,
            d: *mut c_void,
            e: *mut c_void,
            f: u32,
            g: *mut Blob,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn LocalFree(p: *mut c_void) -> *mut c_void;
    }
    let source = Blob {
        size: input.len() as u32,
        data: input.as_ptr() as *mut u8,
    };
    let entropy = Blob {
        size: entropy.len() as u32,
        data: entropy.as_ptr() as *mut u8,
    };
    let mut output = Blob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &source,
                std::ptr::null(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(ServerStoreError::Ownership);
    }
    let result = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    unsafe {
        LocalFree(output.data.cast());
    }
    Ok(result)
}

#[cfg(unix)]
fn protect(input: &[u8], _: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    // Linux relies on same-user ownership and private 0700/0600 storage; it does not encrypt at rest.
    Ok(input.to_vec())
}
#[cfg(unix)]
fn unprotect(input: &[u8], _: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    Ok(input.to_vec())
}
#[cfg(not(any(unix, windows)))]
fn protect(_: &[u8], _: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    Err(ServerStoreError::UnsupportedGame)
}
#[cfg(not(any(unix, windows)))]
fn unprotect(_: &[u8], _: &[u8]) -> Result<Vec<u8>, ServerStoreError> {
    Err(ServerStoreError::UnsupportedGame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::ServerContext;
    use serde_json::json;

    fn store(root: &Path) -> ServerStore {
        fs::write(
            root.join("games.json"),
            serde_json::to_vec(&json!({
                "active_game_space_id": "alpha",
                "last_opened_game_space_id": "alpha",
                "game_spaces": [{"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0}]
            })).unwrap(),
        ).unwrap();
        ServerStore::new(ServerContext::capture(root, Some("alpha")).unwrap()).unwrap()
    }

    #[test]
    fn references_and_values_enforce_the_storage_boundary() {
        assert!(validate_reference(&format!("secret_{}", "a".repeat(32))).is_ok());
        assert!(validate_reference(&"A".repeat(32)).is_err());
        assert!(validate_reference(&format!("secret_{}", "A".repeat(32))).is_err());
        assert!(validate_reference("../credentials").is_err());
        assert!(validate_value(&"x".repeat(MAX_SECRET_BYTES)).is_ok());
        assert!(validate_value(&"x".repeat(MAX_SECRET_BYTES + 1)).is_err());
        assert!(validate_value("bad\0value").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_protection_round_trips_as_private_plaintext() {
        let value = b"synthetic-secret";
        let protected = protect(value, b"space-bound entropy").unwrap();
        assert_eq!(
            unprotect(&protected, b"space-bound entropy").unwrap(),
            value
        );
        assert_eq!(protected, value);
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_directory_requires_current_owner_and_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("space");
        let credentials = space.join("server/credentials/server_x");
        fs::create_dir_all(&credentials).unwrap();
        for dir in [
            &space,
            &space.join("server"),
            &space.join("server/credentials"),
            &credentials,
        ] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        check_private_secret_dirs(&credentials, &space).unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            check_private_secret_dirs(&credentials, &space),
            Err(ServerStoreError::UnsafePath)
        );
    }

    #[test]
    fn copying_a_credential_record_to_another_root_grants_no_binding() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let first = store(first_root.path());
        let second = store(second_root.path());
        let id = ServerInstanceId::new();
        let reference = format!("secret_{}", "c".repeat(32));
        first
            .store_config_secret(&id, &reference, "first synthetic value")
            .unwrap();
        second
            .store_config_secret(&id, &reference, "second synthetic value")
            .unwrap();
        fs::copy(
            first.secret_path(&id, &reference),
            second.secret_path(&id, &reference),
        )
        .unwrap();
        assert_eq!(
            second.resolve_config_secret(&id, &reference),
            Err(ServerStoreError::Ownership)
        );
    }

    #[test]
    fn protected_store_creates_and_resolves_an_immutable_secret() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let id = ServerInstanceId::new();
        let reference = format!("secret_{}", "b".repeat(32));
        store
            .store_config_secret(&id, &reference, "synthetic secret")
            .unwrap();
        assert_eq!(
            store.resolve_config_secret(&id, &reference).unwrap(),
            "synthetic secret"
        );
        assert!(
            store
                .store_config_secret(&id, &reference, "different")
                .is_err()
        );
        let directory = store.context().space_dir().join("server/credentials");
        #[cfg(any(unix, windows))]
        foxy_supervisor::control::handoff::ProtectedDirectory::open(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(&directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let instance_dir = directory.join(id.as_str());
            assert_eq!(
                fs::symlink_metadata(instance_dir.join(format!("{reference}.json")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_dpapi_round_trip_uses_entropy() {
        let encrypted = protect(b"synthetic-secret", b"test entropy").unwrap();
        assert_eq!(
            unprotect(&encrypted, b"test entropy").unwrap(),
            b"synthetic-secret"
        );
        assert!(unprotect(&encrypted, b"different entropy").is_err());
    }
}
