//! The proxy every Foxy HTTP client uses. System mode is reqwest's own default
//! (the Windows proxy settings and the `HTTP_PROXY` / `HTTPS_PROXY` variables),
//! so only None and Manual change how a client is built.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use log::{info, warn};

use crate::ui::types::ProxyMode;

/// Shown instead of a stored proxy password.
pub const REDACTED_PASSWORD: &str = "<redacted>";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub url: String,
    pub username: String,
    pub password: String,
    pub bypass: String,
}

static PROXY: RwLock<Option<ProxySettings>> = RwLock::new(None);
/// Bumped on every proxy change so shared clients know to rebuild.
static PROXY_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Apply the user's proxy to clients built from now on. Logs the mode only:
/// the address names the user's network and the password is a secret.
pub fn set_proxy_settings(settings: ProxySettings) {
    let mut guard = PROXY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = guard.clone().unwrap_or_default();
    if current == settings {
        return;
    }
    match settings.mode {
        ProxyMode::System => info!("Proxy set to system settings"),
        ProxyMode::None => info!("Proxy set to direct connections"),
        ProxyMode::Manual => match validate_proxy_url(&settings.url) {
            Ok(()) => info!(
                "Proxy set to a manual proxy (authentication={} bypass_entries={})",
                !settings.username.trim().is_empty(),
                bypass_entry_count(&settings.bypass)
            ),
            Err(reason) => warn!(
                "Manual proxy address is not usable ({}); using system proxy settings",
                reason
            ),
        },
    }
    *guard = Some(settings);
    PROXY_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// A blocking client shared across many requests, rebuilt when the proxy
/// changes so a long-lived client never keeps the proxy it started with.
pub struct SharedBlockingClient {
    configure: fn(reqwest::blocking::ClientBuilder) -> reqwest::blocking::ClientBuilder,
    slot: Mutex<Option<(u64, reqwest::blocking::Client)>>,
}

impl SharedBlockingClient {
    pub const fn new(
        configure: fn(reqwest::blocking::ClientBuilder) -> reqwest::blocking::ClientBuilder,
    ) -> Self {
        Self {
            configure,
            slot: Mutex::new(None),
        }
    }

    pub fn client(&self) -> reqwest::blocking::Client {
        let generation = PROXY_GENERATION.load(Ordering::Relaxed);
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((built_for, client)) = slot.as_ref()
            && *built_for == generation
        {
            return client.clone();
        }
        let client = (self.configure)(blocking_client_builder())
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        *slot = Some((generation, client.clone()));
        client
    }
}

fn current_settings() -> ProxySettings {
    PROXY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Async client builder with the configured proxy applied.
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    match client_proxy(&current_settings()) {
        ClientProxy::System => builder,
        ClientProxy::Direct => builder.no_proxy(),
        ClientProxy::Manual(proxy) => builder.proxy(*proxy),
    }
}

/// Blocking client builder with the configured proxy applied.
pub fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    let builder = reqwest::blocking::Client::builder();
    match client_proxy(&current_settings()) {
        ClientProxy::System => builder,
        ClientProxy::Direct => builder.no_proxy(),
        ClientProxy::Manual(proxy) => builder.proxy(*proxy),
    }
}

/// Async client with the configured proxy and reqwest's defaults otherwise.
pub fn client() -> reqwest::Client {
    client_builder().build().unwrap_or_default()
}

/// Blocking client with the configured proxy and reqwest's defaults otherwise.
pub fn blocking_client() -> reqwest::blocking::Client {
    blocking_client_builder()
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// Why a manual proxy address cannot be used, as an `en.json` key, or `Ok`.
pub fn validate_proxy_url(url: &str) -> Result<(), &'static str> {
    normalized_proxy_url(url).map(|_| ())
}

/// Replace a stored proxy password in serialized settings, for output that a
/// user may paste into a bug report.
pub fn redact_settings_value(value: &mut serde_json::Value) {
    if let Some(password) = value.get_mut("proxy_password")
        && password.as_str().is_some_and(|text| !text.is_empty())
    {
        *password = serde_json::Value::String(REDACTED_PASSWORD.to_string());
    }
}

enum ClientProxy {
    System,
    Direct,
    Manual(Box<reqwest::Proxy>),
}

/// An unusable manual address falls back to the system settings, the behavior
/// before the setting existed; the settings view shows why.
fn client_proxy(settings: &ProxySettings) -> ClientProxy {
    match settings.mode {
        ProxyMode::System => ClientProxy::System,
        ProxyMode::None => ClientProxy::Direct,
        ProxyMode::Manual => manual_proxy(settings).map_or(ClientProxy::System, |proxy| {
            ClientProxy::Manual(Box::new(proxy))
        }),
    }
}

fn manual_proxy(settings: &ProxySettings) -> Result<reqwest::Proxy, &'static str> {
    let url = normalized_proxy_url(&settings.url)?;
    let mut proxy = reqwest::Proxy::all(url.as_str()).map_err(|_| INVALID_ADDRESS)?;
    let username = settings.username.trim();
    if !username.is_empty() {
        proxy = proxy.basic_auth(username, &settings.password);
    }
    let bypass = settings.bypass.trim();
    if !bypass.is_empty() {
        proxy = proxy.no_proxy(reqwest::NoProxy::from_string(bypass));
    }
    Ok(proxy)
}

const MISSING_ADDRESS: &str = "Enter a proxy address, for example http://proxy.example:8080.";
const UNSUPPORTED_SCHEME: &str = "Proxy addresses must start with http:// or https://.";
const CREDENTIALS_IN_ADDRESS: &str =
    "Enter the proxy user name and password in their own fields, not in the address.";
const INVALID_ADDRESS: &str = "This is not a valid proxy address.";

/// The address with a scheme, `http://` when the user typed only `host:port`.
fn normalized_proxy_url(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(MISSING_ADDRESS);
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let parsed = reqwest::Url::parse(&with_scheme).map_err(|_| INVALID_ADDRESS)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UNSUPPORTED_SCHEME);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(CREDENTIALS_IN_ADDRESS);
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(INVALID_ADDRESS);
    }
    Ok(with_scheme)
}

fn bypass_entry_count(bypass: &str) -> usize {
    bypass
        .split(',')
        .filter(|entry| !entry.trim().is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(url: &str) -> ProxySettings {
        ProxySettings {
            mode: ProxyMode::Manual,
            url: url.to_string(),
            ..ProxySettings::default()
        }
    }

    #[test]
    fn a_bare_host_and_port_is_an_http_proxy() {
        assert_eq!(
            normalized_proxy_url(" proxy.example:8080 ").as_deref(),
            Ok("http://proxy.example:8080")
        );
        assert_eq!(
            normalized_proxy_url("https://proxy.example").as_deref(),
            Ok("https://proxy.example")
        );
    }

    #[test]
    fn unusable_addresses_say_why() {
        assert_eq!(validate_proxy_url("  "), Err(MISSING_ADDRESS));
        assert_eq!(
            validate_proxy_url("socks5://127.0.0.1:1080"),
            Err(UNSUPPORTED_SCHEME)
        );
        assert_eq!(
            validate_proxy_url("http://user:secret@proxy.example:8080"),
            Err(CREDENTIALS_IN_ADDRESS)
        );
        assert_eq!(validate_proxy_url("http://"), Err(INVALID_ADDRESS));
    }

    #[test]
    fn modes_choose_the_client_proxy() {
        assert!(matches!(
            client_proxy(&ProxySettings::default()),
            ClientProxy::System
        ));
        let direct = ProxySettings {
            mode: ProxyMode::None,
            ..ProxySettings::default()
        };
        assert!(matches!(client_proxy(&direct), ClientProxy::Direct));
        assert!(matches!(
            client_proxy(&manual("127.0.0.1:3128")),
            ClientProxy::Manual(_)
        ));
    }

    #[test]
    fn an_unusable_manual_address_falls_back_to_system_settings() {
        assert!(matches!(
            client_proxy(&manual("socks5://127.0.0.1:1080")),
            ClientProxy::System
        ));
        assert!(matches!(client_proxy(&manual("")), ClientProxy::System));
    }

    #[test]
    fn manual_proxies_accept_credentials_and_bypass_lists() {
        let settings = ProxySettings {
            username: "proxy-user".to_string(),
            password: "secret".to_string(),
            bypass: "localhost, 127.0.0.1, .example.invalid".to_string(),
            ..manual("http://127.0.0.1:3128")
        };
        assert!(manual_proxy(&settings).is_ok());
        assert_eq!(bypass_entry_count(&settings.bypass), 3);
    }

    #[test]
    fn redaction_hides_only_a_stored_password() {
        let mut value = serde_json::json!({ "proxy_password": "secret", "proxy_url": "x" });
        redact_settings_value(&mut value);
        assert_eq!(value["proxy_password"], REDACTED_PASSWORD);
        assert_eq!(value["proxy_url"], "x");

        let mut empty = serde_json::json!({ "proxy_password": "" });
        redact_settings_value(&mut empty);
        assert_eq!(empty["proxy_password"], "");
    }
}
