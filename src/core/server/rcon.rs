//! RCon login fixture. The password is not rendered and no socket is opened.

#![cfg_attr(not(test), allow(dead_code))]

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RconLogin {
    password: String,
}

impl RconLogin {
    pub(crate) fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
        }
    }

    pub(crate) fn opens_socket() -> bool {
        false
    }

    pub(crate) fn a2s_info_request() -> &'static [u8] {
        b"\xff\xff\xff\xffTSource Engine Query\0"
    }

    pub(crate) fn transmit_query() -> bool {
        false
    }
}

impl std::fmt::Debug for RconLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RconLogin([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_rcon_fixture_redacts_the_password_and_opens_no_socket() {
        let password = "token-secret-value";
        let login = RconLogin::new(password);
        let rendered = format!("{login:?}");
        assert_eq!(rendered, "RconLogin([redacted])");
        assert!(!rendered.contains(password));
        assert!(!RconLogin::opens_socket());
        assert!(RconLogin::a2s_info_request().starts_with(&[0xff, 0xff, 0xff, 0xff]));
        assert!(!RconLogin::transmit_query());
    }
}
