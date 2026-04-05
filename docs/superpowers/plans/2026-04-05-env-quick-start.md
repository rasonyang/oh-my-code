# Env Quick-Start Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow `oh-my-code` to be configured from three env vars (`API_KEY` + `BASE_URL` + `MODEL`) alone, with wire format and auth style auto-detected from the URL, while keeping the existing `config.toml` multi-provider workflow intact as a fallback.

**Architecture:** A pure `detect_backend(base_url) -> DetectedBackend` function decides wire format + auth style from the URL. At startup, `AppConfig::load` reads the three env vars; if all three are set, it synthesizes an in-memory provider entry named `"env"` using the detection result, sets `default.provider = "env"` and `default.model = $MODEL`, and stores the *routing* provider name (`claude` / `minimax-anthropic` / `openai`) in a new serde-skipped `routing_name: Option<String>` field on `ProviderConfig`. `cli.rs` reads `routing_name` when calling `create_provider`, falling back to `default.provider` when it's `None`. Existing provider entries and routing are untouched.

**Tech Stack:** Rust 2021, tokio, serde, toml, `url` (already transitive via reqwest; will be declared explicitly). No new runtime dependencies beyond `url`.

**Spec:** `docs/superpowers/specs/2026-04-05-env-quick-start-design.md`

---

## File Structure

**Modify:**
- `Cargo.toml` — add `url = "2"` to `[dependencies]` (already transitive via reqwest; declaring explicitly is good hygiene).
- `src/config.rs` — add `DetectedBackend` struct, `detect_backend` pure function, `read_env_quick_start` helper, `routing_name` field on `ProviderConfig`, synthesis step in `AppConfig::load`. Inline unit tests added to the existing `#[cfg(test)] mod tests` block.
- `src/cli.rs` — use `provider_config.routing_name.clone().unwrap_or_else(|| config.default.provider.clone())` when calling `create_provider`.
- `.env.example` — restructure to lead with the three-var quick-start section; keep the existing per-vendor key vars as a documented fallback.

**Not modified:**
- `src/model/mod.rs` — `create_provider` still dispatches on the wire-format name it receives. No new match arm needed; it gets `"claude"` / `"minimax-anthropic"` / `"openai"` via the `routing_name` indirection.
- `src/model/claude.rs`, `src/model/openai.rs` — adapter internals untouched.
- `config/default.toml` — still ships all five provider entries as the TOML-driven fallback.

---

## Task 1: `detect_backend` pure function + exhaustive unit tests

**Files:**
- Modify: `src/config.rs` (add the function + tests)
- Modify: `Cargo.toml` (declare `url` dependency)

- [ ] **Step 1: Add `url` to `Cargo.toml`**

Edit `Cargo.toml`. Under `[dependencies]`, after `dotenvy = "0.15"`:

```toml
url = "2"
```

Run: `cargo build 2>&1 | tail -5`
Expected: clean build, no new downloads (it's already in the lockfile via reqwest).

- [ ] **Step 2: Write failing tests for `detect_backend`**

Add this module-private enum and struct near the top of `src/config.rs`, below the existing `AuthStyle` definition:

```rust
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
```

Then add these tests to the existing `#[cfg(test)] mod tests` block in the
same file (at the bottom, after the existing `test_default_config_serializes`):

```rust
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test detect_ -- --nocapture`
Expected: compile error — `detect_backend` and `DetectedBackend` not yet visible in the test module. (They are `pub(crate)` and the test module is the same crate, so the error will be "cannot find function `detect_backend`".)

- [ ] **Step 4: Implement `detect_backend`**

Add this function to `src/config.rs`, immediately below the `DetectedBackend` struct added in Step 2:

```rust
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
        }
        if parsed.path().to_ascii_lowercase().contains("/anthropic") {
            return DetectedBackend {
                routing_name: "minimax-anthropic",
                auth_style: AuthStyle::Bearer,
            };
        }
    }

    DetectedBackend {
        routing_name: "openai",
        auth_style: AuthStyle::Bearer,
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test detect_ -- --nocapture`
Expected: all 6 `detect_*` tests pass.

- [ ] **Step 6: Run the full test suite to confirm no regressions**

Run: `cargo test`
Expected: all tests pass (97 pre-existing + 6 new = 103).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs
git commit -m "config: add detect_backend for URL-based wire format detection

Pure function + DetectedBackend struct. Rules: api.anthropic.com →
claude+XApiKey; any path containing /anthropic → minimax-anthropic+Bearer;
otherwise → openai+Bearer. Malformed URLs fall through to openai. 6 unit
tests covering case-insensitivity, trailing slashes, and malformed input."
```

---

## Task 2: Add `routing_name` field to `ProviderConfig`

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write a failing test asserting `routing_name` defaults to `None` for TOML-loaded providers**

Add this test to the existing `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
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
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test toml_loaded_providers_have_no_routing_name`
Expected: compile error — `routing_name` field doesn't exist on `ProviderConfig`.

- [ ] **Step 3: Add the field**

In `src/config.rs`, update the `ProviderConfig` struct. Current form:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
    pub base_url: String,
    #[serde(default)]
    pub auth_style: AuthStyle,
}
```

New form:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
    pub base_url: String,
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
```

Then update every existing `ProviderConfig { .. }` literal inside
`default_config` to include `routing_name: None,` as the fourth field. There
are five literals (`claude`, `openai`, `zhipu`, `minimax`, `minimax-anthropic`).

- [ ] **Step 4: Run the new test plus the full config test suite**

Run: `cargo test config::`
Expected: all tests pass. In particular:
- `toml_loaded_providers_have_no_routing_name` passes.
- `test_parse_default_config` still passes — `#[serde(skip)]` means TOML
  parsing yields `None` without requiring the field in the TOML file.
- `test_default_config_serializes` still passes — serialize drops the
  field, deserialize restores it as `None`, which matches the original.

- [ ] **Step 5: Run the entire test suite**

Run: `cargo test`
Expected: all 104 tests pass (previous 103 + this one).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "config: add serde-skipped routing_name field to ProviderConfig

Field is None for all TOML-loaded providers (the config-map key stays
authoritative). Will be Some(...) only for the synthetic env provider
built in the next task. #[serde(skip)] keeps it out of TOML entirely."
```

---

## Task 3: `read_env_quick_start` + `AppConfig::load` synthesis

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing tests for env-driven synthesis**

Env vars are process-global, so the tests must serialize access. Add a test-module-local mutex and a helper that sets/unsets vars cleanly. Append to the existing `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
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
fn env_quick_start_minimax_anthropic_url_uses_bearer_and_claude_routing() {
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
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test env_quick_start`
Expected: compile error — `apply_env_quick_start` not defined.

- [ ] **Step 3: Implement `apply_env_quick_start` and wire it into `load`**

Add this function to `src/config.rs`, below `detect_backend`:

```rust
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
    let api_key = std::env::var("API_KEY").ok().filter(|s| !s.is_empty());
    let base_url = std::env::var("BASE_URL").ok().filter(|s| !s.is_empty());
    let model = std::env::var("MODEL").ok().filter(|s| !s.is_empty());

    let (Some(_api_key), Some(base_url), Some(model)) = (api_key, base_url, model) else {
        return;
    };

    let backend = detect_backend(&base_url);
    cfg.providers.insert(
        "env".to_string(),
        ProviderConfig {
            api_key_env: "API_KEY".to_string(),
            base_url,
            auth_style: backend.auth_style,
            routing_name: Some(backend.routing_name.to_string()),
        },
    );
    cfg.default.provider = "env".to_string();
    cfg.default.model = model;
}
```

Then find `AppConfig::load` in the same file. Its current body constructs the
config (from file if present, from defaults otherwise) and returns it. Add a
single call to `apply_env_quick_start` immediately before the final `Ok(config)`:

```rust
pub fn load() -> Result<Self> {
    let path = Self::config_path()?;
    let mut config = if path.exists() {
        // ... existing load-from-file branch ...
    } else {
        // ... existing create-defaults branch ...
    };

    apply_env_quick_start(&mut config);

    Ok(config)
}
```

If your editor jumps to a different shape (e.g. `config` was previously
immutable), change `let config = ...` to `let mut config = ...` on that line.

- [ ] **Step 4: Run the env_quick_start tests**

Run: `cargo test env_quick_start`
Expected: all 6 new tests pass.

- [ ] **Step 5: Run the full config test suite**

Run: `cargo test config::`
Expected: all config tests pass, including the pre-existing
`test_parse_default_config`, `test_default_config_serializes`, and the
`test_provider_config_auth_style_*` tests.

- [ ] **Step 6: Run the entire test suite**

Run: `cargo test`
Expected: 110 tests pass (previous 104 + 6 new env tests).

- [ ] **Step 7: Commit**

```bash
git add src/config.rs
git commit -m "config: synthesize 'env' provider when API_KEY/BASE_URL/MODEL are set

AppConfig::load now calls apply_env_quick_start after loading config.toml.
If all three env vars are set (and non-empty), it inserts an 'env' provider
entry using detect_backend to choose auth_style and routing_name, then
points default.provider and default.model at it. Partial/empty inputs leave
the config untouched. Covered by 6 tests using a module-level mutex to
serialize env mutation across parallel test runs."
```

---

## Task 4: Route `create_provider` via `routing_name` in `cli.rs`

**Files:**
- Modify: `src/cli.rs`

- [ ] **Step 1: Read the current state of the call site**

Run: `sed -n '22,35p' src/cli.rs`

It should look like:

```rust
impl Repl {
    pub fn new(config: AppConfig) -> Result<Self> {
        let api_key = config.resolve_api_key()?;
        let provider_config = config.active_provider_config()?;
        let provider_name = config.default.provider.clone();
        let base_url = provider_config.base_url.clone();

        let auth_style = provider_config.auth_style;
        let provider = model::create_provider(&provider_name, api_key, base_url, auth_style)?;
```

(Line numbers may drift slightly; the `let provider_name = ...` and the
`model::create_provider(...)` call are the two lines of interest.)

- [ ] **Step 2: Replace the `provider_name` binding**

Change:

```rust
let provider_name = config.default.provider.clone();
```

to:

```rust
// When the synthetic "env" provider is active, routing_name carries the
// real wire-format name ("claude" / "minimax-anthropic" / "openai"). For
// TOML-loaded providers routing_name is None and the config-map key IS the
// routing name.
let provider_name = provider_config
    .routing_name
    .clone()
    .unwrap_or_else(|| config.default.provider.clone());
```

Leave the `model::create_provider(&provider_name, ...)` call below it
unchanged — it now transparently receives the right name in both paths.

- [ ] **Step 3: Build the crate**

Run: `cargo build 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all 110 tests pass.

- [ ] **Step 5: Manual smoke test of the TOML path (no env vars set)**

Make sure none of `API_KEY`, `BASE_URL`, or `MODEL` is exported in your
current shell, then:

Run: `unset API_KEY BASE_URL MODEL && cargo test config::test_parse_default_config`
Expected: passes — confirms the TOML-driven code path still works end to
end after the routing change.

- [ ] **Step 6: Clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -20`
Expected: no new warnings from the cli.rs change.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs
git commit -m "cli: route create_provider via ProviderConfig.routing_name

Uses the routing_name override when present (env-quick-start synthesized
provider), otherwise falls back to the default.provider map key. No
change for TOML-only workflows; unlocks the env path to route to any of
the three adapters without collision on the 'env' map key."
```

---

## Task 5: Update `.env.example` with the three-var quick-start section

**Files:**
- Modify: `.env.example`

- [ ] **Step 1: Replace `.env.example` with the restructured version**

Overwrite `.env.example` at the repo root with this exact content:

```bash
# =============================================================================
# .env.example — template for local development secrets
#
# Copy this file to .env and fill in the values you need. .env is gitignored;
# never commit real secrets. .env.example itself is tracked as the template.
#
# Two ways to configure oh-my-code:
#
#   Quick start (three vars, no config.toml editing):
#     Set API_KEY + BASE_URL + MODEL below. oh-my-code auto-detects the wire
#     format and auth style from BASE_URL:
#       host = api.anthropic.com        → Anthropic API, x-api-key header
#       path contains /anthropic        → Anthropic-compatible, Bearer header
#       anything else                   → OpenAI-compatible, Bearer header
#
#   Multi-provider (more control):
#     Leave API_KEY/BASE_URL/MODEL empty and set the per-vendor key vars at
#     the bottom. Select the active provider in
#     ~/.config/oh-my-code/config.toml under [default].
#
# Precedence: when ALL THREE of API_KEY, BASE_URL, and MODEL are set (and
# non-empty), they win over config.toml. If any is missing, config.toml drives
# provider selection.
# =============================================================================

# ---- Quick start ------------------------------------------------------------

API_KEY=
BASE_URL=
MODEL=

# Examples you can paste into the three above:
#
#   Anthropic:
#     API_KEY=sk-ant-...
#     BASE_URL=https://api.anthropic.com
#     MODEL=claude-sonnet-4-5
#
#   MiniMax (Anthropic-compatible endpoint):
#     API_KEY=sk-cp-...
#     BASE_URL=https://api.minimaxi.com/anthropic
#     MODEL=MiniMax-M2.7-highspeed
#
#   OpenAI:
#     API_KEY=sk-...
#     BASE_URL=https://api.openai.com
#     MODEL=gpt-4o

# ---- Multi-provider fallback ------------------------------------------------
# Used when you prefer configuring providers in ~/.config/oh-my-code/config.toml
# instead of the three quick-start vars above. Only the provider you select in
# config.toml's [default] needs its key set.

ANTHROPIC_API_KEY=
ANTHROPIC_AUTH_TOKEN=
OPENAI_API_KEY=
ZHIPU_API_KEY=
MINIMAX_API_KEY=
```

- [ ] **Step 2: Verify the file is well-formed**

Run: `head -20 .env.example && echo "---" && wc -l .env.example`
Expected: the header block prints and line count is ~55.

- [ ] **Step 3: Verify `.env` files are still ignored and `.env.example` is still tracked**

Run: `git check-ignore -v .env 2>/dev/null || echo ".env not ignored (WRONG)"; git check-ignore .env.example && echo ".env.example ignored (WRONG)" || echo ".env.example tracked (correct)"`
Expected:
```
.gitignore:<line>:.env	.env
.env.example tracked (correct)
```

- [ ] **Step 4: Commit**

```bash
git add .env.example
git commit -m "docs(.env.example): lead with three-var quick-start, keep multi-provider fallback

Documents the API_KEY/BASE_URL/MODEL path with example values for Anthropic,
MiniMax Anthropic-compatible, and OpenAI. Preserves per-vendor key vars as
the fallback for users driving provider selection from config.toml."
```

---

## Task 6: Final verification — whole-branch checks

**Files:** (none modified; verification only)

- [ ] **Step 1: Full test suite, no env vars set**

Run: `unset API_KEY BASE_URL MODEL && cargo test`
Expected: 110 passed, 0 failed.

- [ ] **Step 2: Full test suite, env vars set (covering the new path)**

Run:

```bash
API_KEY=sk-fake-for-test-only \
BASE_URL=https://api.openai.com \
MODEL=gpt-4o \
cargo test
```

Expected: 110 passed, 0 failed. (The env-mutating tests in Task 3 save and
restore their own vars via `with_env_vars`, so they're unaffected by these
outer values.)

- [ ] **Step 3: Release build**

Run: `cargo build --release 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 4: Clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -20`
Expected: no new warnings.

- [ ] **Step 5: Smoke test the TOML path has not regressed**

Run: `cargo test test_parse_default_config -- --nocapture`
Expected: passes. This test is baked via `include_str!("../config/default.toml")`, so a pass confirms that the shipped TOML still parses cleanly into the new `ProviderConfig` shape (with the serde-skipped `routing_name` field defaulting to `None` for all five entries).

- [ ] **Step 6: No commit**

Task 6 is verification-only. If all steps pass, the branch is ready to merge.
If any step fails, diagnose and fix in a new task before proceeding.

---

## Verification summary

After all tasks complete:

- `cargo test` (no env) → 110 passing tests: ~97 pre-existing + 6 from Task 1 (`detect_*`) + 1 from Task 2 (`toml_loaded_providers_have_no_routing_name`) + 6 from Task 3 (`env_quick_start_*`).
- `detect_backend` is exhaustively unit-tested for Anthropic/MiniMax-Anthropic/OpenAI/case-insensitivity/malformed-input cases.
- `apply_env_quick_start` is tested for all-three-set (three URL flavors), none-set, partial-set, and empty-string-set cases.
- `.env.example` documents the quick-start path with pasteable examples and preserves the multi-provider fallback.
- All existing workflows — `config.toml`-driven multi-provider selection, `/model` runtime switching, the `live_minimax_anthropic_roundtrip` opt-in test — continue to work unchanged because the new code path is purely additive and only activates when all three env vars are present.

## Files touched

| File | Tasks | Why |
| --- | --- | --- |
| `Cargo.toml` | 1 | Declare `url = "2"` explicitly |
| `src/config.rs` | 1, 2, 3 | `DetectedBackend`, `detect_backend`, `routing_name` field, `apply_env_quick_start`, `AppConfig::load` wiring, 13 new tests |
| `src/cli.rs` | 4 | Use `routing_name` when calling `create_provider` |
| `.env.example` | 5 | Quick-start documentation |
