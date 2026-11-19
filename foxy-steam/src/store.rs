use crate::{AuthError, PreparedCredential, Secret};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

const SCHEMA: u32 = 1;
const MAX_RECORD: usize = 8192;
const MAX_TOKEN: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    context: String,
    alias: String,
    ciphertext: Vec<u8>,
}

pub struct SessionStore {
    directory: PathBuf,
    context: String,
}

pub struct StoreGuard {
    directory: PathBuf,
    context: String,
    _lock: File,
}

impl SessionStore {
    pub fn new(root: PathBuf, scope: String) -> Result<Self, AuthError> {
        if scope.is_empty()
            || scope.len() > 128
            || !scope
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(AuthError::InvalidInput);
        }
        let absolute = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .map_err(|_| AuthError::Storage)?
                .join(root)
        };
        let directory = lexical_normalize(&absolute)?;
        let mut binding = b"Foxy Steam session store v1\0".to_vec();
        binding.extend_from_slice(directory.to_string_lossy().as_bytes());
        binding.push(0);
        binding.extend_from_slice(scope.as_bytes());
        Ok(Self {
            directory,
            context: blake3::hash(&binding).to_hex().to_string(),
        })
    }

    pub fn inspect(&self, alias: &str) -> Result<bool, AuthError> {
        validate_alias(alias)?;
        match read_record(&self.directory, alias, &self.context) {
            Ok(_) => Ok(true),
            Err(AuthError::AuthenticationRequired) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn load(&self, alias: &str) -> Result<PreparedCredential, AuthError> {
        validate_alias(alias)?;
        read_record(&self.directory, alias, &self.context)
    }

    pub fn lock(&self) -> Result<StoreGuard, AuthError> {
        validate_existing_chain(&self.directory)?;
        ensure_private_namespace(&self.directory)?;
        let path = self.directory.join(".lock");
        #[cfg(windows)]
        {
            let protected =
                foxy_supervisor::control::handoff::ProtectedDirectory::open(&self.directory)
                    .map_err(|_| AuthError::Storage)?;
            match protected.publish_new(".lock", &[]) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    protected.read(".lock").map_err(|_| AuthError::Storage)?;
                }
                Err(_) => return Err(AuthError::Storage),
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(path).map_err(|_| AuthError::Storage)?;
        verify_private_file(&file)?;
        file.try_lock().map_err(|_| AuthError::Busy)?;
        Ok(StoreGuard {
            directory: self.directory.clone(),
            context: self.context.clone(),
            _lock: file,
        })
    }
}

impl StoreGuard {
    pub fn load(&self, alias: &str) -> Result<PreparedCredential, AuthError> {
        validate_alias(alias)?;
        read_record(&self.directory, alias, &self.context)
    }

    pub fn save(&self, alias: &str, credential: &PreparedCredential) -> Result<(), AuthError> {
        validate_alias(alias)?;
        let account = credential.account_name.expose();
        let token = credential.refresh_token.expose();
        if account.is_empty()
            || account.len() > MAX_TOKEN
            || token.is_empty()
            || token.len() > MAX_TOKEN
            || account.contains('\0')
            || token.contains('\0')
        {
            return Err(AuthError::InvalidInput);
        }
        let plaintext = serde_json::to_vec(&(account, token)).map_err(|_| AuthError::Storage)?;
        let ciphertext = protect(&plaintext, &entropy(&self.context, alias))?;
        let bytes = serde_json::to_vec(&Record {
            schema: SCHEMA,
            context: self.context.clone(),
            alias: alias.to_owned(),
            ciphertext,
        })
        .map_err(|_| AuthError::Storage)?;
        if bytes.len() > MAX_RECORD {
            return Err(AuthError::InvalidInput);
        }
        atomic_replace(&self.directory, alias, &self.context, &bytes)
    }

    pub fn forget(&self, alias: &str) -> Result<(), AuthError> {
        validate_alias(alias)?;
        let bytes = match read_file(&self.directory, alias) {
            Ok(bytes) => bytes,
            Err(AuthError::AuthenticationRequired) => return Ok(()),
            Err(error) => return Err(error),
        };
        validate_record(&bytes, alias, &self.context)?;
        #[cfg(unix)]
        {
            let path = record_path(&self.directory, alias);
            fs::remove_file(path).map_err(|_| AuthError::Storage)?;
            sync_directory(&self.directory).map_err(|_| AuthError::StorageUncertain)
        }
        #[cfg(windows)]
        {
            remove_record_windows(&self.directory, alias, &bytes)
        }
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, AuthError> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => (),
            Component::ParentDir => {
                if !out.pop() {
                    return Err(AuthError::InvalidInput);
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if !out.is_absolute() {
        return Err(AuthError::InvalidInput);
    }
    Ok(out)
}

fn validate_alias(alias: &str) -> Result<(), AuthError> {
    if alias.is_empty()
        || alias.len() > 64
        || !alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(AuthError::InvalidInput);
    }
    Ok(())
}

fn entropy(context: &str, alias: &str) -> Vec<u8> {
    let mut value = b"Foxy Steam refresh token v1\0".to_vec();
    value.extend_from_slice(context.as_bytes());
    value.push(0);
    value.extend_from_slice(alias.as_bytes());
    value
}

fn record_path(directory: &Path, alias: &str) -> PathBuf {
    directory.join(format!("{alias}.json"))
}

fn read_record(
    directory: &Path,
    alias: &str,
    context: &str,
) -> Result<PreparedCredential, AuthError> {
    let bytes = read_file(directory, alias)?;
    let record = validate_record(&bytes, alias, context)?;
    let plain = unprotect(&record.ciphertext, &entropy(context, alias))?;
    if plain.len() > MAX_TOKEN * 2 + 32 {
        return Err(AuthError::InvalidStoredSession);
    }
    let (account, token): (String, String) =
        serde_json::from_slice(&plain).map_err(|_| AuthError::InvalidStoredSession)?;
    if account.is_empty()
        || token.is_empty()
        || account.len() > MAX_TOKEN
        || token.len() > MAX_TOKEN
        || account.contains('\0')
        || token.contains('\0')
    {
        return Err(AuthError::InvalidStoredSession);
    }
    Ok(PreparedCredential {
        account_name: Secret::new(account),
        refresh_token: Secret::new(token),
    })
}

fn validate_record(bytes: &[u8], alias: &str, context: &str) -> Result<Record, AuthError> {
    let record: Record =
        serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidStoredSession)?;
    if record.schema != SCHEMA || record.alias != alias || record.context != context {
        return Err(AuthError::InvalidStoredSession);
    }
    Ok(record)
}

fn read_file(directory: &Path, alias: &str) -> Result<Vec<u8>, AuthError> {
    validate_existing_chain(directory)?;
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuthError::AuthenticationRequired);
        }
        Err(_) => return Err(AuthError::Storage),
        Ok(_) => (),
    }
    check_namespace(directory, false)?;
    #[cfg(windows)]
    {
        let name = format!("{alias}.json");
        foxy_supervisor::control::handoff::ProtectedDirectory::open(directory)
            .map_err(|_| AuthError::Storage)?
            .read(&name)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AuthError::AuthenticationRequired
                } else {
                    AuthError::Storage
                }
            })
    }
    #[cfg(unix)]
    {
        let path = record_path(directory, alias);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AuthError::AuthenticationRequired
            } else {
                AuthError::Storage
            }
        })?;
        verify_private_file(&file)?;
        let mut bytes = Vec::new();
        file.take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| AuthError::Storage)?;
        if bytes.len() > MAX_RECORD {
            return Err(AuthError::InvalidStoredSession);
        }
        Ok(bytes)
    }
}

fn validate_existing_chain(directory: &Path) -> Result<(), AuthError> {
    let mut path = PathBuf::new();
    for component in directory.components() {
        path.push(component.as_os_str());
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(AuthError::Storage);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(AuthError::Storage);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(AuthError::Storage),
        }
    }
    Ok(())
}

fn check_namespace(directory: &Path, create: bool) -> Result<(), AuthError> {
    if create {
        ensure_private_namespace(directory)
    } else {
        #[cfg(any(unix, windows))]
        {
            foxy_supervisor::control::handoff::ProtectedDirectory::open(directory)
                .map_err(|_| AuthError::Storage)?;
        }
        Ok(())
    }
}

fn ensure_private_namespace(directory: &Path) -> Result<(), AuthError> {
    if directory.exists() {
        return check_namespace(directory, false);
    }
    let parent = directory.parent().ok_or(AuthError::Storage)?;
    ensure_parent_chain(parent)?;
    #[cfg(windows)]
    {
        foxy_supervisor::control::handoff::ProtectedDirectory::create(directory)
            .map_err(|_| AuthError::Storage)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .map_err(|_| AuthError::Storage)?;
        foxy_supervisor::control::handoff::ProtectedDirectory::open(directory)
            .map_err(|_| AuthError::Storage)?;
    }
    Ok(())
}

fn ensure_parent_chain(parent: &Path) -> Result<(), AuthError> {
    if parent.exists() {
        #[cfg(unix)]
        {
            let metadata = fs::symlink_metadata(parent).map_err(|_| AuthError::Storage)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(AuthError::Storage);
            }
        }
        return Ok(());
    }
    let ancestor = parent.parent().ok_or(AuthError::Storage)?;
    ensure_parent_chain(ancestor)?;
    #[cfg(windows)]
    {
        foxy_supervisor::control::handoff::ProtectedDirectory::create(parent)
            .map_err(|_| AuthError::Storage)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(parent)
            .map_err(|_| AuthError::Storage)?;
    }
    Ok(())
}

fn verify_private_file(file: &File) -> Result<(), AuthError> {
    let metadata = file.metadata().map_err(|_| AuthError::Storage)?;
    if !metadata.is_file() {
        return Err(AuthError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(AuthError::Storage);
        }
    }
    Ok(())
}

fn atomic_replace(
    directory: &Path,
    alias: &str,
    context: &str,
    bytes: &[u8],
) -> Result<(), AuthError> {
    check_namespace(directory, false)?;
    match read_file(directory, alias) {
        Ok(old) => {
            validate_record(&old, alias, context)?;
        }
        Err(AuthError::AuthenticationRequired) => (),
        Err(error) => return Err(error),
    }
    let target = record_path(directory, alias);
    let temp_name = format!(".{alias}-{}.tmp", std::process::id());
    #[cfg(windows)]
    {
        let protected = foxy_supervisor::control::handoff::ProtectedDirectory::open(directory)
            .map_err(|_| AuthError::Storage)?;
        protected
            .publish_new(&temp_name, bytes)
            .map_err(|_| AuthError::Storage)?;
        let temp = directory.join(&temp_name);
        let result = replace_file(&temp, &target);
        if result.is_err() {
            let _ = protected.remove_exact(&temp_name, bytes);
        }
        result?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = directory.join(&temp_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
            .map_err(|_| AuthError::Storage)?;
        let staged = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| AuthError::Storage);
        if staged.is_err() {
            let _ = fs::remove_file(&temp);
        }
        staged?;
        fs::rename(&temp, &target).map_err(|_| {
            let _ = fs::remove_file(&temp);
            AuthError::Storage
        })?;
        sync_directory(directory).map_err(|_| AuthError::StorageUncertain)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(unix)]
fn replace_file(source: &Path, target: &Path) -> Result<(), AuthError> {
    fs::rename(source, target).map_err(|_| AuthError::Storage)
}

#[cfg(windows)]
fn remove_record_windows(directory: &Path, alias: &str, bytes: &[u8]) -> Result<(), AuthError> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let tombstone = format!(".forget-{}.tmp", std::process::id());
    let source: Vec<u16> = record_path(directory, alias)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let destination: Vec<u16> = directory
        .join(&tombstone)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x8) } == 0 {
        return Err(AuthError::Storage);
    }
    let protected = foxy_supervisor::control::handoff::ProtectedDirectory::open(directory)
        .map_err(|_| AuthError::StorageUncertain)?;
    protected
        .remove_exact(&tombstone, bytes)
        .map_err(|_| AuthError::StorageUncertain)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> Result<(), AuthError> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(AuthError::Storage)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn protect(input: &[u8], entropy: &[u8]) -> Result<Vec<u8>, AuthError> {
    dpapi(input, entropy, true)
}
#[cfg(windows)]
fn unprotect(input: &[u8], entropy: &[u8]) -> Result<Vec<u8>, AuthError> {
    dpapi(input, entropy, false)
}

#[cfg(windows)]
fn dpapi(input: &[u8], entropy: &[u8], encrypt: bool) -> Result<Vec<u8>, AuthError> {
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
    let entropy_blob = Blob {
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
                &entropy_blob,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                &entropy_blob,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(AuthError::InvalidStoredSession);
    }
    let value = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    unsafe {
        LocalFree(output.data.cast());
    }
    Ok(value)
}

#[cfg(unix)]
fn protect(input: &[u8], _: &[u8]) -> Result<Vec<u8>, AuthError> {
    Ok(input.to_vec())
}
#[cfg(unix)]
fn unprotect(input: &[u8], _: &[u8]) -> Result<Vec<u8>, AuthError> {
    Ok(input.to_vec())
}
#[cfg(not(any(unix, windows)))]
fn protect(_: &[u8], _: &[u8]) -> Result<Vec<u8>, AuthError> {
    Err(AuthError::Storage)
}
#[cfg(not(any(unix, windows)))]
fn unprotect(_: &[u8], _: &[u8]) -> Result<Vec<u8>, AuthError> {
    Err(AuthError::Storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "foxy-steam-store-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }
    fn credential(token: &str) -> PreparedCredential {
        PreparedCredential {
            account_name: Secret::new("synthetic-account".into()),
            refresh_token: Secret::new(token.into()),
        }
    }

    #[test]
    fn round_trip_rotation_scope_and_forget() {
        let root = temp_root();
        let store =
            SessionStore::new(root.join("server/steam-auth"), "space_alpha".into()).unwrap();
        assert!(!store.inspect("operator").unwrap());
        {
            let guard = store.lock().unwrap();
            guard
                .save("operator", &credential("synthetic-token-one"))
                .unwrap();
            assert_eq!(
                guard.load("operator").unwrap().refresh_token.expose(),
                "synthetic-token-one"
            );
            guard
                .save("operator", &credential("synthetic-token-two"))
                .unwrap();
        }
        assert_eq!(
            store.load("operator").unwrap().refresh_token.expose(),
            "synthetic-token-two"
        );
        let foreign =
            SessionStore::new(root.join("server/steam-auth"), "space_beta".into()).unwrap();
        assert!(matches!(
            foreign.load("operator"),
            Err(AuthError::InvalidStoredSession)
        ));
        store.lock().unwrap().forget("operator").unwrap();
        assert!(!store.inspect("operator").unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_record_and_alias_are_rejected() {
        let root = temp_root();
        let store = SessionStore::new(root.join("auth"), "space_alpha".into()).unwrap();
        let guard = store.lock().unwrap();
        assert!(matches!(guard.load("../bad"), Err(AuthError::InvalidInput)));
        guard
            .save("operator", &credential("synthetic-token"))
            .unwrap();
        fs::write(record_path(&store.directory, "operator"), b"broken").unwrap();
        assert!(matches!(
            guard.load("operator"),
            Err(AuthError::InvalidStoredSession)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_record_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = temp_root();
        let store = SessionStore::new(root.join("auth"), "space_alpha".into()).unwrap();
        store.lock().unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"synthetic").unwrap();
        symlink(outside, record_path(&store.directory, "operator")).unwrap();
        assert!(matches!(store.load("operator"), Err(AuthError::Storage)));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_ancestor_cannot_create_private_storage_outside_scope() {
        use std::os::unix::fs::symlink;
        let root = temp_root();
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        let store =
            SessionStore::new(root.join("linked/new-parent/auth"), "space_alpha".into()).unwrap();
        assert!(matches!(store.lock(), Err(AuthError::Storage)));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn example_native_steam_session_record_parses() {
        let record: Record = serde_json::from_str(include_str!(
            "../../examples/json/server_backend/native_steam_session_record.json"
        ))
        .expect("parse example Steam session record");
        assert_eq!(record.schema, SCHEMA);
        assert!(!record.context.is_empty());
        assert!(!record.alias.is_empty());
        assert!(!record.ciphertext.is_empty());
    }
}
