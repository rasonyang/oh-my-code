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
///   2. URL path contains `/anthropic` (case-insensitive) → minimax-anthropic + Bearer
///   3. Otherwise → openai + Bearer
///
/// Malformed URLs fall through to rule 3; the subsequent HTTP request will
/// fail cleanly via the adapter's normal error path.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
    pub base_url: String,
    #[serde(default)]
    pub auth_style: AuthStyle,
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
        let base = dirs::config_dir().ok_or_else(|| anyhow!("Could not determine config directory"))?;
        Ok(base.join("oh-my-code"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file at {}", path.display()))?;
            Self::load_from_str(&content)
        } else {
            let config = Self::default_config();
            let dir = Self::config_dir()?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create config directory {}", dir.display()))?;
            let content = toml::to_string_pretty(&config)
                .context("Failed to serialize default config")?;
            std::fs::write(&path, &content)
                .with_context(|| format!("Failed to write default config to {}", path.display()))?;
            Ok(config)
        }
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
            },
        );
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                api_key_env: "OPENAI_API_KEY".to_string(),
                base_url: "https://api.openai.com".to_string(),
                auth_style: AuthStyle::XApiKey,
            },
        );
        providers.insert(
            "zhipu".to_string(),
            ProviderConfig {
                api_key_env: "ZHIPU_API_KEY".to_string(),
                base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
                auth_style: AuthStyle::XApiKey,
            },
        );
        providers.insert(
            "minimax".to_string(),
            ProviderConfig {
                api_key_env: "MINIMAX_API_KEY".to_string(),
                base_url: "https://api.minimax.chat/v1".to_string(),
                auth_style: AuthStyle::XApiKey,
            },
        );
        providers.insert(
            "minimax-anthropic".to_string(),
            ProviderConfig {
                api_key_env: "ANTHROPIC_AUTH_TOKEN".to_string(),
                base_url: "https://api.minimaxi.com/anthropic".to_string(),
                auth_style: AuthStyle::Bearer,
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
}
