mod inventory;
mod model;

pub(crate) use inventory::provider_coverage;

pub use model::{
    ReforgerProviderCoverage, ReforgerSessionIntent, ReforgerSessionInventory,
    ReforgerSessionLaunch, ReforgerSessionPlan, ReforgerSessionSummary,
};

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSaveRef {
    pub save_id: String,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveError {
    Filename,
    Identity,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn session_save(save_id: &str) -> Result<SessionSaveRef, SaveError> {
    if valid_uuid(save_id) {
        Ok(SessionSaveRef {
            save_id: save_id.to_owned(),
        })
    } else {
        Err(SaveError::Identity)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn session_save_from_filename(name: &str) -> Result<SessionSaveRef, SaveError> {
    let _ = name;
    Err(SaveError::Filename)
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_session_identity_comes_from_metadata_not_a_filename() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(session_save(id).unwrap().save_id, id);
        assert_eq!(
            session_save_from_filename("01234567-89ab-cdef-0123-456789abcdef.save"),
            Err(SaveError::Filename)
        );
        assert_eq!(session_save("autosave.pbo"), Err(SaveError::Identity));
    }
}
