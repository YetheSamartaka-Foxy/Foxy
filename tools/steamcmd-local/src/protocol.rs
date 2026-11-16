#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Password,
    GuardCode,
    MobileApproval,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Password,
    Guard,
    Login,
}

#[derive(Default)]
pub(super) struct Protocol {
    buffer: Vec<u8>,
    escape: u8,
    password_sent: bool,
    mobile_reported: bool,
}

impl Protocol {
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Action> {
        for &byte in bytes {
            if self.escape == 1 {
                self.escape = if byte == b'[' { 2 } else { 0 };
            } else if self.escape == 2 {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = 0;
                }
            } else if byte == 0x1b {
                self.escape = 1;
            } else {
                self.buffer.push(byte.to_ascii_lowercase());
            }
        }
        if self.buffer.len() > 32 * 1024 {
            self.buffer.drain(..self.buffer.len() - 16 * 1024);
        }
        let text = String::from_utf8_lossy(&self.buffer);
        let action = if ["invalid password", "invalidpassword"]
            .iter()
            .any(|word| text.contains(word))
        {
            Some(Action::Failed(Failure::Password))
        } else if [
            "invalid login auth code",
            "two-factor code mismatch",
            "expired login auth code",
            "that steam guard code was invalid",
        ]
        .iter()
        .any(|word| text.contains(word))
        {
            Some(Action::Failed(Failure::Guard))
        } else if ["login failure", "failed to log"]
            .iter()
            .any(|word| text.contains(word))
        {
            Some(Action::Failed(Failure::Login))
        } else if !self.password_sent && text.contains("password:") {
            self.password_sent = true;
            Some(Action::Password)
        } else if text.contains("steam guard code:")
            || text.contains("two-factor code:")
            || text.contains("enter the current code")
        {
            Some(Action::GuardCode)
        } else if !self.mobile_reported && text.contains("waiting for confirmation") {
            self.mobile_reported = true;
            Some(Action::MobileApproval)
        } else {
            None
        };
        if action.is_some() {
            self.buffer.fill(0);
            self.buffer.clear();
        }
        action
    }
}

#[test]
fn fragmented_prompts_and_invalid_codes_are_distinguished_from_acknowledgments() {
    let mut protocol = Protocol::default();
    assert_eq!(protocol.feed(b"Pass\x1b[0"), None);
    assert_eq!(protocol.feed(b"mword:"), Some(Action::Password));
    assert_eq!(protocol.feed(b"Steam Guard code provided.\n"), None);
    assert_eq!(protocol.feed(b"Steam Guard code:"), Some(Action::GuardCode));
    assert_eq!(
        protocol.feed(b"That Steam Guard code was invalid."),
        Some(Action::Failed(Failure::Guard))
    );
}

#[test]
fn status_text_never_grants_authentication_and_mobile_approval_remains_explicit() {
    let mut protocol = Protocol::default();
    assert_eq!(
        protocol.feed(b"Waiting for user info...NOT OK\nAnother operation...OK\nSteam>"),
        None
    );
    assert_eq!(protocol.feed(b"Waiting for user info...OK\nSteam>"), None);
    assert_eq!(
        protocol.feed(b"Waiting for confirmation..."),
        Some(Action::MobileApproval)
    );
    assert_eq!(protocol.feed(b"Waiting for confirmation..."), None);
    assert_eq!(protocol.feed(&vec![b'x'; 64 * 1024]), None);
    assert!(protocol.buffer.len() <= 16 * 1024);
}
