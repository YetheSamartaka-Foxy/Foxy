#![cfg_attr(not(test), allow(dead_code))]

/// A caller-supplied revision. A mismatch is refused; it is not a reason to retry.
pub fn revision_matches(expected: u64, current: u64) -> bool {
    expected == current && expected != 0
}

/// Recorded intent never becomes permission to kill, even after a deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAuthorization {
    RecordedIntent,
    ExplicitForce,
}

pub fn may_escalate_stop(authorization: StopAuthorization, deadline_elapsed: bool) -> bool {
    let _ = deadline_elapsed;
    matches!(authorization, StopAuthorization::ExplicitForce)
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretRef {
    kind: &'static str,
}

impl SecretRef {
    pub fn new(kind: &'static str) -> Self {
        Self { kind }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

impl std::fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretRef([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_mismatch_is_refused_and_zero_is_never_current() {
        assert!(revision_matches(4, 4));
        assert!(!revision_matches(4, 5));
        assert!(!revision_matches(0, 0));
    }

    #[test]
    fn a_stop_deadline_does_not_grant_force() {
        assert!(!may_escalate_stop(StopAuthorization::RecordedIntent, true));
        assert!(!may_escalate_stop(StopAuthorization::RecordedIntent, false));
        assert!(may_escalate_stop(StopAuthorization::ExplicitForce, false));
    }

    #[test]
    fn secret_debug_text_does_not_include_a_token() {
        let token = "token-secret-value";
        let secret = SecretRef::new("rcon");
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "SecretRef([redacted])");
        assert!(!rendered.contains(token));
        assert_eq!(secret.kind(), "rcon");
    }
}
