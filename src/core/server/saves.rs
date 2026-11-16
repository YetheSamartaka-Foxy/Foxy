#![cfg_attr(not(test), allow(dead_code))]

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSaveRef {
    pub save_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveError {
    Filename,
    Identity,
}

pub(crate) fn session_save(save_id: &str) -> Result<SessionSaveRef, SaveError> {
    let bytes = save_id.as_bytes();
    let uuid = bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit());
    if uuid {
        Ok(SessionSaveRef {
            save_id: save_id.to_string(),
        })
    } else {
        Err(SaveError::Identity)
    }
}

pub(crate) fn session_save_from_filename(name: &str) -> Result<SessionSaveRef, SaveError> {
    let _ = name;
    Err(SaveError::Filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_file_name_is_not_a_session_identity() {
        assert_eq!(
            session_save_from_filename("autosave.pbo"),
            Err(SaveError::Filename)
        );
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(session_save(id).unwrap().save_id, id);
        assert!(session_save("autosave.pbo").is_err());
    }
}
