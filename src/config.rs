use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub default: DefaultConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub search: SearchConfig,
    pub session: SessionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    #[default]
    XApiKey,
    Bearer,
}

/// Wire format + auth header style derived from a URL. Used only by the
/// env-quick-start synthesis path; normal config.toml providers hardcode
/// their auth_style and routing via the provider-name match in create_provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DetectedBackend {
    /// Name passed to `create_provider` to select the adapter. One of
    /// `"claude"`, `"minimax-anthropic"`, `"openai"`.
    pub routing_name: &'static str,
    pub auth_style: AuthStyle,
}

/// Decide wire format + auth style from a base URL.
///
/// Rules (first match wins):
///   1. Host is `api.anthropic.com` (case-insensitive) → claude + XApiKey
///   2. Host is present AND path contains `/anthropic` (case-insensitive) →
///      minimax-anthropic + Bearer
///   3. Otherwise → openai + Bearer
///
/// URLs that fail to parse, or parse successfully but have no host (e.g.
/// `file://`), fall through to rule 3. The resulting HTTP request — if any —
/// will fail cleanly via the adapter's normal error path.
pub(crate) fn detect_backend(base_url: &str) -> DetectedBackend {
    if let Ok(parsed) = url::Url::parse(base_url) {
        if let Some(host) = parsed.host_str() {
            if host.eq_ignore_ascii_case("api.anthropic.com") {
                return DetectedBackend {
                    routing_name: "claude",
                    auth_style: AuthStyle::XApiKey,
                };
            }
            if parsed.path().to_ascii_lowercase().contains("/anthropic") {
                return DetectedBackend {
                    routing_name: "minimax-anthropic",
                    auth_style: AuthStyle::Bearer,
                };
            }
        }
    }

    DetectedBackend {
        routing_name: "openai",
        auth_style: AuthStyle::Bearer,
    }
}

/// Read `API_KEY`, `BASE_URL`, and `MODEL` from the environment. If all three
/// are set and non-empty, synthesize an in-memory provider entry named `"env"`
/// into `cfg.providers`, set `cfg.default.provider = "env"`, and set
/// `cfg.default.model` from `MODEL`. If any one is missing or empty, leave
/// `cfg` untouched.
///
/// This is the env-quick-start layer documented in
/// `docs/superpowers/specs/2026-04-05-env-quick-start-design.md`. It runs after
/// config.toml has been loaded so it strictly overrides TOML when active.
pub(crate) fn apply_env_quick_start(cfg: &mut AppConfig) {
    // Trim + empty check: whitespace-only values are treated as unset. A user
    // who typed `API_KEY=   ` in .env almost certainly meant "not set".
    let read = |name: &str| -> Option<String> {
        std::env::var(name).ok().filter(|s| !s.trim().is_empty())
    };
    let api_key = read("API_KEY");
    let base_url = read("BASE_URL");
    let model = read("MODEL");

    let (Some(_api_key), Some(base_url), Some(model)) = (api_key, base_url, model) else {
        // Partial config is almost certainly a user mistake. Warn loudly so
        // they know why their quick-start setup isn't taking effect.
        let missing: Vec<&str> = [
            ("API_KEY", std::env::var("API_KEY").ok().filter(|s| !s.trim().is_empty()).is_some()),
            ("BASE_URL", std::env::var("BASE_URL").ok().filter(|s| !s.trim().is_empty()).is_some()),
            ("MODEL", std::env::var("MODEL").ok().filter(|s| !s.trim().is_empty()).is_some()),
        ]
        .iter()
        .filter_map(|(name, set)| if *set { None } else { Some(*name) })
        .collect();
        let set_count = 3 - missing.len();
        if set_count > 0 {
            eprintln!(
                "oh-my-code: env quick-start inactive — all three of API_KEY, BASE_URL, \
                 and MODEL must be set (got {}/3, missing: {}). Falling back to config.toml.",
                set_count,
                missing.join(", "),
            );
        }
        return;
    };

    let backend = detect_backend(&base_url);
    cfg.providers.insert(
        "env".to_string(),
        ProviderConfig {
            api_key_env: "API_KEY".to_string(),
            base_url,
            api_key: None,
            auth_style: backend.auth_style,
            routing_name: Some(backend.routing_name.to_string()),
        },
    );
    cfg.default.provider = "env".to_string();
    cfg.default.model = model;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key_env: String,
    pub base_url: String,
    /// API key value set directly in config. Takes priority over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_style: AuthStyle,
    /// Routing name passed to `create_provider` when this entry is active.
    /// `None` for normal TOML-loaded providers — in that case the config-map
    /// key IS the routing name. `Some(...)` only for the synthetic "env"
    /// provider built from API_KEY/BASE_URL/MODEL env vars. Never serialized
    /// to TOML.
    #[serde(skip)]
    pub routing_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub ignore_patterns: Vec<String>,
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub storage_dir: String,
}

impl AppConfig {
    pub fn config_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
        Ok(home.join(".config").join("oh-my-code"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let mut config = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file at {}", path.display()))?;
            Self::load_from_str(&content)?
        } else {
            let config = Self::default_config();
            let dir = Self::config_dir()?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create config directory {}", dir.display()))?;
            let content = toml::to_string_pretty(&config)
                .context("Failed to serialize default config")?;
            std::fs::write(&path, &content)
                .with_context(|| format!("Failed to write default config to {}", path.display()))?;
            config
        };

        apply_env_quick_start(&mut config);

        Ok(config)
    }

    pub fn load_from_str(content: &str) -> Result<Self> {
        toml::from_str(content).context("Failed to parse config TOML")
    }

    pub fn default_config() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "claude".to_string(),
            ProviderConfig {
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                auth_style: AuthStyle::XApiKey,
                api_key: None,
                routing_name: None,
            },
        );
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                api_key_env: "OPENAI_API_KEY".to_string(),
                base_url: "https://api.openai.com".to_string(),
                auth_style: AuthStyle::XApiKey,
                api_key: None,
                routing_name: None,
            },
        );
        providers.insert(
            "zhipu".to_string(),
            ProviderConfig {
                api_key_env: "ZHIPU_API_KEY".to_string(),
                base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
                auth_style: AuthStyle::XApiKey,
                api_key: None,
                routing_name: None,
            },
        );
        providers.insert(
            "minimax".to_string(),
            ProviderConfig {
                api_key_env: "MINIMAX_API_KEY".to_string(),
                base_url: "https://api.minimax.chat/v1".to_string(),
                auth_style: AuthStyle::XApiKey,
                api_key: None,
                routing_name: None,
            },
        );
        providers.insert(
            "minimax-anthropic".to_string(),
            ProviderConfig {
                api_key_env: "ANTHROPIC_AUTH_TOKEN".to_string(),
                base_url: "https://api.minimaxi.com/anthropic".to_string(),
                auth_style: AuthStyle::Bearer,
                api_key: None,
                routing_name: None,
            },
        );

        AppConfig {
            default: DefaultConfig {
                provider: "claude".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
            },
            providers,
            search: SearchConfig {
                ignore_patterns: vec![
                    "node_modules".to_string(),
                    ".git".to_string(),
                    "dist".to_string(),
                    "build".to_string(),
                    "target".to_string(),
                ],
                max_results: 500,
            },
            session: SessionConfig {
                storage_dir: "~/.config/oh-my-code/sessions".to_string(),
            },
        }
    }

    pub fn active_provider_config(&self) -> Result<&ProviderConfig> {
        let provider_name = &self.default.provider;
        self.providers
            .get(provider_name)
            .ok_or_else(|| anyhow!("Provider '{}' not found in config", provider_name))
    }

    pub fn resolve_api_key(&self) -> Result<String> {
        let provider = self.active_provider_config()?;
        // Prefer api_key from config file; fall back to env var lookup.
        if let Some(key) = &provider.api_key {
            if !key.is_empty() {
                return Ok(key.clone());
            }
        }
        if provider.api_key_env.is_empty() {
            anyhow::bail!(
                "Provider '{}' has neither 'api_key' nor 'api_key_env' configured",
                self.default.provider
            );
        }
        std::env::var(&provider.api_key_env).with_context(|| {
            format!(
                "Environment variable '{}' not set for provider '{}'",
                provider.api_key_env, self.default.provider
            )
        })
    }

    pub fn resolved_session_dir(&self) -> PathBuf {
        let storage_dir = &self.session.storage_dir;
        if let Some(rest) = storage_dir.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        PathBuf::from(storage_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default_config() {
        let content = include_str!("../config/default.toml");
        let config = AppConfig::load_from_str(content).expect("Should parse default.toml");
        assert_eq!(config.default.provider, "claude");
        assert_eq!(config.providers.len(), 5);
        assert_eq!(config.search.ignore_patterns.len(), 5);

        let mma = config
            .providers
            .get("minimax-anthropic")
            .expect("minimax-anthropic provider must be present");
        assert_eq!(mma.api_key_env, "ANTHROPIC_AUTH_TOKEN");
        assert_eq!(mma.base_url, "https://api.minimaxi.com/anthropic");
        assert_eq!(mma.auth_style, AuthStyle::Bearer);
    }

    #[test]
    fn test_active_provider_config() {
        let config = AppConfig::default_config();
        let provider = config.active_provider_config().expect("Should get active provider");
        assert_eq!(provider.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(provider.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn test_active_provider_missing() {
        let mut config = AppConfig::default_config();
        config.default.provider = "nonexistent".to_string();
        let result = config.active_provider_config();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_resolved_session_dir_tilde() {
        let config = AppConfig::default_config();
        let resolved = config.resolved_session_dir();
        let resolved_str = resolved.to_string_lossy();
        assert!(!resolved_str.starts_with("~/"), "Tilde should be expanded, got: {}", resolved_str);
        assert!(resolved_str.contains(".config/oh-my-code/sessions"));
    }

    #[test]
    fn test_default_config_serializes() {
        let config = AppConfig::default_config();
        let serialized = toml::to_string_pretty(&config).expect("Should serialize");
        let deserialized = AppConfig::load_from_str(&serialized).expect("Should deserialize");
        assert_eq!(deserialized.default.provider, config.default.provider);
        assert_eq!(deserialized.default.model, config.default.model);
        assert_eq!(deserialized.providers.len(), config.providers.len());
        assert_eq!(
            deserialized.search.ignore_patterns,
            config.search.ignore_patterns
        );
        assert_eq!(deserialized.session.storage_dir, config.session.storage_dir);
    }

    fn db(name: &'static str, style: AuthStyle) -> DetectedBackend {
        DetectedBackend { routing_name: name, auth_style: style }
    }

    #[test]
    fn detect_real_anthropic_uses_xapi_key() {
        assert_eq!(detect_backend("https://api.anthropic.com"), db("claude", AuthStyle::XApiKey));
        assert_eq!(detect_backend("https://api.anthropic.com/v1/messages"), db("claude", AuthStyle::XApiKey));
    }

    #[test]
    fn detect_anthropic_host_is_case_insensitive() {
        assert_eq!(detect_backend("https://API.Anthropic.COM"), db("claude", AuthStyle::XApiKey));
        assert_eq!(detect_backend("https://api.ANTHROPIC.com/v1"), db("claude", AuthStyle::XApiKey));
    }

    #[test]
    fn detect_third_party_anthropic_path_uses_bearer() {
        assert_eq!(detect_backend("https://api.minimaxi.com/anthropic"), db("minimax-anthropic", AuthStyle::Bearer));
        assert_eq!(detect_backend("https://api.minimaxi.com/anthropic/"), db("minimax-anthropic", AuthStyle::Bearer));
        assert_eq!(detect_backend("https://api.minimaxi.com/anthropic/v1/messages"), db("minimax-anthropic", AuthStyle::Bearer));
    }

    #[test]
    fn detect_third_party_anthropic_path_is_case_insensitive() {
        assert_eq!(detect_backend("https://api.minimaxi.com/ANTHROPIC"), db("minimax-anthropic", AuthStyle::Bearer));
        assert_eq!(detect_backend("https://corp-proxy.internal/Anthropic/v1"), db("minimax-anthropic", AuthStyle::Bearer));
    }

    #[test]
    fn detect_openai_falls_through_to_bearer() {
        assert_eq!(detect_backend("https://api.openai.com"), db("openai", AuthStyle::Bearer));
        assert_eq!(detect_backend("https://api.openai.com/v1/chat/completions"), db("openai", AuthStyle::Bearer));
        assert_eq!(detect_backend("https://api.zhipu.com/v4"), db("openai", AuthStyle::Bearer));
    }

    #[test]
    fn detect_malformed_url_falls_through_to_openai() {
        assert_eq!(detect_backend("not a url"), db("openai", AuthStyle::Bearer));
        assert_eq!(detect_backend(""), db("openai", AuthStyle::Bearer));
        assert_eq!(detect_backend("http://"), db("openai", AuthStyle::Bearer));
    }

    #[test]
    fn detect_hostless_url_with_anthropic_in_path_falls_through_to_openai() {
        // URLs that parse successfully but have no host (e.g. file://) must not
        // match the /anthropic path rule — only hosted URLs qualify.
        assert_eq!(
            detect_backend("file:///tmp/anthropic/v1"),
            db("openai", AuthStyle::Bearer),
        );
    }

    #[test]
    fn test_provider_config_auth_style_defaults_to_x_api_key() {
        // Legacy TOML without auth_style must still parse; default is XApiKey.
        let content = r#"
[default]
provider = "claude"
model = "claude-sonnet-4-20250514"

[providers.claude]
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"

[search]
ignore_patterns = []
max_results = 100

[session]
storage_dir = "/tmp"
"#;
        let config = AppConfig::load_from_str(content).expect("Should parse");
        let p = config.providers.get("claude").expect("claude provider");
        assert_eq!(p.auth_style, AuthStyle::XApiKey);
    }

    #[test]
    fn test_provider_config_auth_style_bearer() {
        let content = r#"
[default]
provider = "x"
model = "y"

[providers.x]
api_key_env = "TOK"
base_url = "https://example.com"
auth_style = "bearer"

[search]
ignore_patterns = []
max_results = 100

[session]
storage_dir = "/tmp"
"#;
        let config = AppConfig::load_from_str(content).expect("Should parse");
        let p = config.providers.get("x").expect("x provider");
        assert_eq!(p.auth_style, AuthStyle::Bearer);
    }

    #[test]
    fn toml_loaded_providers_have_no_routing_name() {
        let config = AppConfig::default_config();
        for (name, provider) in &config.providers {
            assert!(
                provider.routing_name.is_none(),
                "provider '{}' should have routing_name = None by default (it's only set by env synthesis)",
                name
            );
        }
    }

    use std::sync::Mutex;

    // Single mutex guards every test that mutates env vars. `cargo test` runs
    // tests in parallel by default; without this lock, two env-mutating tests
    // would trample each other.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_vars<F: FnOnce()>(vars: &[(&str, Option<&str>)], test: F) {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test));
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    #[test]
    fn env_quick_start_all_three_set_activates_synthetic_provider_anthropic() {
        with_env_vars(
            &[
                ("API_KEY", Some("sk-ant-test-xyz")),
                ("BASE_URL", Some("https://api.anthropic.com")),
                ("MODEL", Some("claude-sonnet-4-5")),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                apply_env_quick_start(&mut cfg);

                assert_eq!(cfg.default.provider, "env");
                assert_eq!(cfg.default.model, "claude-sonnet-4-5");
                let env_provider = cfg.providers.get("env").expect("env provider must be synthesized");
                assert_eq!(env_provider.api_key_env, "API_KEY");
                assert_eq!(env_provider.base_url, "https://api.anthropic.com");
                assert_eq!(env_provider.auth_style, AuthStyle::XApiKey);
                assert_eq!(env_provider.routing_name.as_deref(), Some("claude"));
            },
        );
    }

    #[test]
    fn env_quick_start_minimax_anthropic_url_uses_bearer_and_minimax_anthropic_routing() {
        with_env_vars(
            &[
                ("API_KEY", Some("sk-cp-test-abc")),
                ("BASE_URL", Some("https://api.minimaxi.com/anthropic")),
                ("MODEL", Some("MiniMax-M2.7-highspeed")),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                apply_env_quick_start(&mut cfg);

                assert_eq!(cfg.default.provider, "env");
                assert_eq!(cfg.default.model, "MiniMax-M2.7-highspeed");
                let env_provider = cfg.providers.get("env").unwrap();
                assert_eq!(env_provider.auth_style, AuthStyle::Bearer);
                assert_eq!(env_provider.routing_name.as_deref(), Some("minimax-anthropic"));
            },
        );
    }

    #[test]
    fn env_quick_start_openai_url_uses_bearer_and_openai_routing() {
        with_env_vars(
            &[
                ("API_KEY", Some("sk-test")),
                ("BASE_URL", Some("https://api.openai.com")),
                ("MODEL", Some("gpt-4o")),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                apply_env_quick_start(&mut cfg);

                assert_eq!(cfg.default.provider, "env");
                let env_provider = cfg.providers.get("env").unwrap();
                assert_eq!(env_provider.auth_style, AuthStyle::Bearer);
                assert_eq!(env_provider.routing_name.as_deref(), Some("openai"));
            },
        );
    }

    #[test]
    fn env_quick_start_none_set_leaves_config_untouched() {
        with_env_vars(
            &[
                ("API_KEY", None),
                ("BASE_URL", None),
                ("MODEL", None),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                let original_provider = cfg.default.provider.clone();
                let original_model = cfg.default.model.clone();
                let original_provider_count = cfg.providers.len();

                apply_env_quick_start(&mut cfg);

                assert_eq!(cfg.default.provider, original_provider);
                assert_eq!(cfg.default.model, original_model);
                assert_eq!(cfg.providers.len(), original_provider_count);
                assert!(!cfg.providers.contains_key("env"));
            },
        );
    }

    #[test]
    fn env_quick_start_partial_set_leaves_config_untouched() {
        with_env_vars(
            &[
                ("API_KEY", Some("sk-test")),
                ("BASE_URL", Some("https://api.openai.com")),
                ("MODEL", None),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                let original_provider = cfg.default.provider.clone();
                apply_env_quick_start(&mut cfg);
                assert_eq!(cfg.default.provider, original_provider);
                assert!(!cfg.providers.contains_key("env"));
            },
        );
    }

    #[test]
    fn env_quick_start_whitespace_only_vars_treated_as_unset() {
        with_env_vars(
            &[
                ("API_KEY", Some("   ")),
                ("BASE_URL", Some("\t\t")),
                ("MODEL", Some("  \n  ")),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                let original_provider = cfg.default.provider.clone();
                apply_env_quick_start(&mut cfg);
                assert_eq!(
                    cfg.default.provider, original_provider,
                    "whitespace-only env vars should not trigger synthesis"
                );
                assert!(!cfg.providers.contains_key("env"));
            },
        );
    }

    #[test]
    fn env_quick_start_empty_string_vars_treated_as_unset() {
        with_env_vars(
            &[
                ("API_KEY", Some("")),
                ("BASE_URL", Some("https://api.openai.com")),
                ("MODEL", Some("gpt-4o")),
            ],
            || {
                let mut cfg = AppConfig::default_config();
                let original_provider = cfg.default.provider.clone();
                apply_env_quick_start(&mut cfg);
                assert_eq!(
                    cfg.default.provider, original_provider,
                    "empty API_KEY should not trigger synthesis"
                );
                assert!(!cfg.providers.contains_key("env"));
            },
        );
    }
}
