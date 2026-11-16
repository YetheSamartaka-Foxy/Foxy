use crate::leases::{Admission, InstallationLocks, directory_chain, ordinary};
use crate::session::SessionIdentity;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub mod witness;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupBinding {
    pub receipt_id: String,
    pub group_id: String,
    pub context_key: String,
}

const MAX_RECORD_BYTES: u64 = 16 * 1024;
const MAX_CONSUMERS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerRecord {
    pub schema_version: u32,
    pub installation_key: String,
    pub session: SessionIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupBinding>,
}

impl ConsumerRecord {
    pub(crate) fn validates(&self) -> bool {
        let id = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        };
        self.session.validate().is_ok()
            && !self.installation_key.is_empty()
            && match (&self.cleanup, self.schema_version) {
                (None, 1) => true,
                (Some(binding), 2) => {
                    binding.receipt_id.len() == 32
                        && binding
                            .receipt_id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                        && id(&binding.group_id)
                        && binding.context_key.len() == 64
                        && binding
                            .context_key
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }
                _ => false,
            }
    }
}

pub(crate) struct Registration {
    path: PathBuf,
    bytes: Vec<u8>,
    receipt: Option<std::fs::File>,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Installation consumer registrations are invalid or unverified.",
    )
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(winapi::um::winbase::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

pub(crate) fn read_record(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !ordinary(&metadata) || !metadata.is_file() {
        return Err(invalid());
    }
    let file = options().read(true).open(path)?;
    let metadata = file.metadata()?;
    if !ordinary(&metadata) || !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(invalid());
    }
    let mut bytes = vec![];
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}

fn record_name(record: &ConsumerRecord) -> io::Result<String> {
    let bytes = serde_json::to_vec(record).map_err(|_| invalid())?;
    Ok(format!("{}.json", blake3::hash(&bytes).to_hex()))
}

impl InstallationLocks {
    pub fn inspect_consumers(&self) -> io::Result<Vec<ConsumerRecord>> {
        self.revalidate()?;
        match directory_chain(&self.namespace, false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let directory = self.namespace.join("consumers");
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        directory_chain(&directory, false)?;
        let mut records = vec![];
        for entry in fs::read_dir(&directory)? {
            if records.len() >= MAX_CONSUMERS {
                return Err(invalid());
            }
            let entry = entry?;
            let record: ConsumerRecord =
                serde_json::from_slice(&read_record(&entry.path())?).map_err(|_| invalid())?;
            record.session.validate()?;
            if !record.validates()
                || record.installation_key != self.coordination_key()?
                || entry.file_name() != std::ffi::OsStr::new(&record_name(&record)?)
            {
                return Err(invalid());
            }
            records.push(record);
        }
        records.sort_by_key(|record| record_name(record).unwrap_or_default());
        Ok(records)
    }

    pub fn coordination_key(&self) -> io::Result<String> {
        self.revalidate()?;
        self.namespace
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
            .ok_or_else(invalid)
    }
}

impl Registration {
    pub(crate) fn create(
        admission: &Admission<'_>,
        identity: &SessionIdentity,
    ) -> io::Result<Self> {
        Self::create_inner(admission, identity, None)
    }

    pub(crate) fn create_witnessed(
        admission: &Admission<'_>,
        identity: &SessionIdentity,
        group_id: &str,
        context_key: &str,
    ) -> io::Result<Self> {
        let binding = CleanupBinding {
            receipt_id: format!("{:032x}", rand::random::<u128>()),
            group_id: group_id.into(),
            context_key: context_key.into(),
        };
        Self::create_inner(admission, identity, Some(binding))
    }

    fn create_inner(
        admission: &Admission<'_>,
        identity: &SessionIdentity,
        cleanup: Option<CleanupBinding>,
    ) -> io::Result<Self> {
        identity.validate()?;
        let installation = admission.installation;
        admission.check_consumer_slot(&identity.game_space_id, &identity.instance_id)?;
        let directory = installation.namespace.join("consumers");
        directory_chain(&directory, true)?;
        let record = ConsumerRecord {
            schema_version: if cleanup.is_some() { 2 } else { 1 },
            installation_key: installation.coordination_key()?,
            session: identity.clone(),
            cleanup,
        };
        if !record.validates() {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&record).map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let receipt = if record.cleanup.is_some() {
            Some(witness::arm(installation, &record, &bytes)?)
        } else {
            None
        };
        let path = directory.join(record_name(&record)?);
        let mut file = options().write(true).create_new(true).open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(Self {
            path,
            bytes,
            receipt,
        })
    }

    pub(crate) fn receipt(&self) -> io::Result<&std::fs::File> {
        self.receipt.as_ref().ok_or_else(invalid)
    }

    pub(crate) fn retire(&mut self) -> io::Result<()> {
        self.receipt.take();
        directory_chain(self.path.parent().ok_or_else(invalid)?, false)?;
        if read_record(&self.path)? != self.bytes {
            return Err(invalid());
        }
        fs::remove_file(&self.path)
    }

    pub(crate) fn retire_unspawned(&mut self) -> io::Result<()> {
        let record: ConsumerRecord = serde_json::from_slice(&self.bytes).map_err(|_| invalid())?;
        if record.cleanup.is_some() {
            witness::discard_unspawned_pending(&self.path, &record, &self.bytes)?;
        }
        self.retire()
    }
}

impl Admission<'_> {
    pub fn check_consumer_slot(&self, game_space_id: &str, instance_id: &str) -> io::Result<()> {
        let records = self.installation.inspect_consumers()?;
        if records.len() >= MAX_CONSUMERS {
            return Err(invalid());
        }
        if records.iter().any(|record| {
            record.session.game_space_id == game_space_id
                && record.session.instance_id == instance_id
        }) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "An instance consumer registration already exists; cleanup must be verified.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
