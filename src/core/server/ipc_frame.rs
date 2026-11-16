//! Bounded local control-frame checks.
//!
//! These functions never open a named pipe, a Unix socket, or any other
//! transport. A successful check is not owner transfer.

#![cfg_attr(not(test), allow(dead_code))]

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
pub const MAX_SPACE_ID_BYTES: usize = 128;
const MAX_SEEN_REQUESTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlFrame {
    pub version: u16,
    pub space_id: String,
    pub owner_generation: u64,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    UnsupportedVersion,
    EmptySpace,
    SpaceTooLong,
    ZeroGeneration,
    Stale,
    ZeroRequest,
    Oversized,
    Replay,
    TooManyRequests,
}

/// Remembers `(owner_generation, request_id)` pairs that have already been accepted.
#[derive(Clone, Debug, Default)]
pub struct RequestWindow {
    seen: Vec<(u64, u64)>,
}

impl RequestWindow {
    pub fn accept(
        &mut self,
        frame: &ControlFrame,
        expected_space: &str,
        expected_generation: u64,
    ) -> Result<(), FrameError> {
        validate_frame(frame, expected_space, expected_generation)?;
        let key = (frame.owner_generation, frame.request_id);
        if self.seen.contains(&key) {
            return Err(FrameError::Replay);
        }
        if self.seen.len() >= MAX_SEEN_REQUESTS {
            return Err(FrameError::TooManyRequests);
        }
        self.seen.push(key);
        Ok(())
    }
}

pub fn validate_frame(
    frame: &ControlFrame,
    expected_space: &str,
    expected_generation: u64,
) -> Result<(), FrameError> {
    if frame.version != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    if frame.space_id.is_empty() {
        return Err(FrameError::EmptySpace);
    }
    if frame.space_id.len() > MAX_SPACE_ID_BYTES {
        return Err(FrameError::SpaceTooLong);
    }
    if frame.owner_generation == 0 {
        return Err(FrameError::ZeroGeneration);
    }
    if frame.space_id != expected_space || frame.owner_generation != expected_generation {
        return Err(FrameError::Stale);
    }
    if frame.request_id == 0 {
        return Err(FrameError::ZeroRequest);
    }
    if frame.payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized);
    }
    Ok(())
}

/// Always false. Frame checks do not open a transport.
pub fn transport_opened() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(space: &str, generation: u64, request_id: u64, payload: Vec<u8>) -> ControlFrame {
        ControlFrame {
            version: PROTOCOL_VERSION,
            space_id: space.into(),
            owner_generation: generation,
            request_id,
            payload,
        }
    }

    #[test]
    fn a_current_bounded_frame_is_accepted_once() {
        let mut window = RequestWindow::default();
        let first = frame("alpha", 4, 9, b"inspect".to_vec());
        assert!(window.accept(&first, "alpha", 4).is_ok());
        assert_eq!(window.accept(&first, "alpha", 4), Err(FrameError::Replay));
        assert!(!transport_opened());
    }

    #[test]
    fn unsupported_empty_zero_and_oversized_frames_are_rejected() {
        let mut bad_version = frame("alpha", 4, 1, Vec::new());
        bad_version.version = 2;
        assert_eq!(
            validate_frame(&bad_version, "alpha", 4),
            Err(FrameError::UnsupportedVersion)
        );
        assert_eq!(
            validate_frame(&frame("", 4, 1, Vec::new()), "alpha", 4),
            Err(FrameError::EmptySpace)
        );
        assert_eq!(
            validate_frame(&frame("alpha", 0, 1, Vec::new()), "alpha", 4),
            Err(FrameError::ZeroGeneration)
        );
        assert_eq!(
            validate_frame(&frame("alpha", 4, 0, Vec::new()), "alpha", 4),
            Err(FrameError::ZeroRequest)
        );
        let huge = frame("alpha", 4, 1, vec![0; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            validate_frame(&huge, "alpha", 4),
            Err(FrameError::Oversized)
        );
    }

    #[test]
    fn a_frame_for_another_space_or_generation_is_stale() {
        let current = frame("alpha", 4, 1, Vec::new());
        assert_eq!(validate_frame(&current, "beta", 4), Err(FrameError::Stale));
        assert_eq!(validate_frame(&current, "alpha", 5), Err(FrameError::Stale));
    }

    #[test]
    fn a_long_space_id_is_rejected_before_it_is_remembered() {
        let mut window = RequestWindow::default();
        let long = frame(&"a".repeat(MAX_SPACE_ID_BYTES + 1), 4, 1, Vec::new());
        assert_eq!(
            window.accept(&long, &long.space_id, 4),
            Err(FrameError::SpaceTooLong)
        );
        assert!(window.seen.is_empty());
    }

    #[test]
    fn the_request_window_fails_closed_when_it_is_full() {
        let mut window = RequestWindow::default();
        for request_id in 1..=MAX_SEEN_REQUESTS as u64 {
            window
                .accept(&frame("alpha", 1, request_id, Vec::new()), "alpha", 1)
                .unwrap();
        }
        assert_eq!(
            window.accept(
                &frame("alpha", 1, MAX_SEEN_REQUESTS as u64 + 1, Vec::new()),
                "alpha",
                1
            ),
            Err(FrameError::TooManyRequests)
        );
    }
}
