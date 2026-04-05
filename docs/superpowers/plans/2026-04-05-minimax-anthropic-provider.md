# MiniMax-via-Anthropic Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route MiniMax's Anthropic-compatible endpoint through the existing `ClaudeProvider` by adding an `AuthStyle` enum on `ProviderConfig`, and verify the integration with both an offline header-routing unit test (via `wiremock`) and an opt-in live integration test.

**Architecture:** `ProviderConfig` gains a closed `AuthStyle` enum field (`x-api-key` default, `bearer` for new entry). A new provider key `minimax-anthropic` routes to `ClaudeProvider` in `create_provider`. `ClaudeProvider` branches on `AuthStyle` when building the request header. The live roundtrip test sits inline in `src/model/claude.rs` behind `OH_MY_CODE_LIVE_TESTS=1`.

**Tech Stack:** Rust (edition 2021), tokio, reqwest, serde, toml, wiremock (new dev-dep).

**Spec:** `docs/superpowers/specs/2026-04-05-minimax-anthropic-provider-design.md`

---

## File Structure

**Modify:**
- `Cargo.toml` — add `wiremock = "0.6"` to `[dev-dependencies]`.
- `src/config.rs` — new `AuthStyle` enum, field on `ProviderConfig`, new provider in `default_config`, updated test assertions.
- `config/default.toml` — new `[providers.minimax-anthropic]` section.
- `src/model/mod.rs` — import `AuthStyle`, extend `create_provider` signature, route `minimax-anthropic` to `ClaudeProvider`.
- `src/model/claude.rs` — `ClaudeProvider` stores and uses `AuthStyle`; offline wiremock unit tests for each header style; opt-in live integration test.
- `src/cli.rs` — forward `provider_config.auth_style` at the single `create_provider` call site.

**No new files.** All changes stay within existing modules, matching the project's flat-module convention.

---

## Task 1: Add `AuthStyle` enum and field on `ProviderConfig` (backward-compatible)

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing test for default `AuthStyle` on existing TOML**

Add this to the `tests` module in `src/config.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --no-run 2>&1 | head -40`
Expected: compile errors — `AuthStyle` not defined, `auth_style` field missing.

- [ ] **Step 3: Add the enum and field**

In `src/config.rs`, add above `ProviderConfig`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    #[default]
    XApiKey,
    Bearer,
}
```

Update `ProviderConfig` to:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
    pub base_url: String,
    #[serde(default)]
    pub auth_style: AuthStyle,
}
```

Then update every `ProviderConfig { .. }` literal inside `default_config` to add
`auth_style: AuthStyle::XApiKey,` (for `claude`, `openai`, `zhipu`, `minimax`).
Do **not** add the new `minimax-anthropic` entry yet — that's Task 2.

- [ ] **Step 4: Run the two new tests**

Run: `cargo test --package oh-my-code test_provider_config_auth_style -- --nocapture`
Expected: both pass.

- [ ] **Step 5: Run the full existing config test suite**

Run: `cargo test --package oh-my-code config::`
Expected: all config tests pass (including `test_parse_default_config`, which still expects 4 providers at this point).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "config: add AuthStyle enum and field on ProviderConfig

Backward-compatible via #[serde(default)]; existing providers continue
to default to XApiKey."
```

---

## Task 2: Add the `minimax-anthropic` provider entry

**Files:**
- Modify: `src/config.rs` (`default_config`, `test_parse_default_config`)
- Modify: `config/default.toml`

- [ ] **Step 1: Update `test_parse_default_config` to expect 5 providers and the new key**

In `src/config.rs`, edit the existing test:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test test_parse_default_config`
Expected: FAIL — `providers.len()` is 4, and `minimax-anthropic` is missing.

- [ ] **Step 3: Add the entry to `config/default.toml`**

Append to `config/default.toml`:

```toml
[providers.minimax-anthropic]
api_key_env = "ANTHROPIC_AUTH_TOKEN"
base_url = "https://api.minimaxi.com/anthropic"
auth_style = "bearer"
```

- [ ] **Step 4: Add the entry to `default_config` in `src/config.rs`**

Inside `default_config`, alongside the other `providers.insert(...)` calls:

```rust
providers.insert(
    "minimax-anthropic".to_string(),
    ProviderConfig {
        api_key_env: "ANTHROPIC_AUTH_TOKEN".to_string(),
        base_url: "https://api.minimaxi.com/anthropic".to_string(),
        auth_style: AuthStyle::Bearer,
    },
);
```

- [ ] **Step 5: Run the config tests**

Run: `cargo test config::`
Expected: all pass — `test_parse_default_config` now sees 5 providers, `test_default_config_serializes` still roundtrips.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs config/default.toml
git commit -m "config: add minimax-anthropic provider entry

Routes MiniMax's Anthropic-compatible endpoint via Bearer auth."
```

---

## Task 3: Add `wiremock` dev-dep and write failing header-routing tests

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/model/claude.rs` (tests module)

- [ ] **Step 1: Add `wiremock` to dev-dependencies**

Edit `Cargo.toml`, under `[dev-dependencies]`:

```toml
[dev-dependencies]
tempfile = "3"
tokio-test = "0.4"
wiremock = "0.6"
```

Run: `cargo build --tests 2>&1 | tail -20`
Expected: compiles successfully with the new dep.

- [ ] **Step 2: Write failing tests asserting each auth style sends the right header**

Add to the `tests` module in `src/model/claude.rs`:

```rust
#[tokio::test]
async fn claude_provider_sends_x_api_key_header_by_default() {
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test-xapi"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"type\":\"message_stop\"}\n\n",
        ))
        .mount(&server)
        .await;

    let provider = ClaudeProvider::new(
        "sk-test-xapi".to_string(),
        server.uri(),
        crate::config::AuthStyle::XApiKey,
    );

    let messages = vec![Message::user("hi")];
    let tools: Vec<ToolDef> = vec![];
    let config = ModelConfig {
        model_id: "claude-test".to_string(),
        max_tokens: 16,
        temperature: 0.0,
    };

    let mut stream = provider
        .send_message(&messages, &tools, &config)
        .await
        .expect("send_message should succeed");

    // Drain; wiremock will panic on drop if the expectation wasn't met.
    while stream.next().await.is_some() {}
}

#[tokio::test]
async fn claude_provider_sends_bearer_header_when_auth_style_bearer() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer sk-test-bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"type\":\"message_stop\"}\n\n",
        ))
        .mount(&server)
        .await;

    let provider = ClaudeProvider::new(
        "sk-test-bearer".to_string(),
        server.uri(),
        crate::config::AuthStyle::Bearer,
    );

    let messages = vec![Message::user("hi")];
    let tools: Vec<ToolDef> = vec![];
    let config = ModelConfig {
        model_id: "minimax-m2.7".to_string(),
        max_tokens: 16,
        temperature: 0.0,
    };

    let mut stream = provider
        .send_message(&messages, &tools, &config)
        .await
        .expect("send_message should succeed");

    while stream.next().await.is_some() {}
}
```

Note: `StreamExt` (providing `next()`) is already in scope via `use futures::stream::{BoxStream, StreamExt};` at the top of the file.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --package oh-my-code claude_provider_sends -- --nocapture`
Expected: FAIL — `ClaudeProvider::new` currently takes only 2 args, not 3. Compilation error.

**Do not commit yet.** The tree will not compile until Task 5 finishes. Tasks 3, 4, and 5 share a single commit at the end of Task 5 to keep every commit green for `git bisect`.

---

## Task 4: Extend `create_provider` signature and route `minimax-anthropic`

**Files:**
- Modify: `src/model/mod.rs`

- [ ] **Step 1: Update the function signature and routing**

Replace the body of `src/model/mod.rs`:

```rust
pub mod types;
pub mod claude;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use types::{Message, ModelConfig, ModelInfo, StreamEvent, ToolDef};

use crate::config::AuthStyle;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn send_message(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &ModelConfig,
    ) -> Result<BoxStream<'static, StreamEvent>>;

    fn name(&self) -> &str;
    fn supported_models(&self) -> Vec<ModelInfo>;
}

pub fn create_provider(
    provider_name: &str,
    api_key: String,
    base_url: String,
    auth_style: AuthStyle,
) -> Result<Box<dyn Provider>> {
    match provider_name {
        "claude" | "minimax-anthropic" => Ok(Box::new(
            claude::ClaudeProvider::new(api_key, base_url, auth_style),
        )),
        "openai" | "zhipu" | "minimax" => Ok(Box::new(
            openai::OpenAIProvider::new(api_key, base_url, provider_name.to_string()),
        )),
        other => anyhow::bail!("unsupported provider: {}", other),
    }
}
```

- [ ] **Step 2: Verify the file compiles against still-old `ClaudeProvider::new`**

Run: `cargo check 2>&1 | tail -30`
Expected: error — `ClaudeProvider::new` still has 2-arg signature. This is expected; Task 5 fixes it.

(Do not commit yet — the tree will not compile until Task 5.)

---

## Task 5: Implement `ClaudeProvider` auth-style branching

**Files:**
- Modify: `src/model/claude.rs`

- [ ] **Step 1: Update the struct, constructor, and `send_message`**

At the top of `src/model/claude.rs`, add the import:

```rust
use crate::config::AuthStyle;
```

Replace the `ClaudeProvider` struct and `impl ClaudeProvider` block:

```rust
pub struct ClaudeProvider {
    api_key: String,
    base_url: String,
    auth_style: AuthStyle,
    client: Client,
}

impl ClaudeProvider {
    pub fn new(api_key: String, base_url: String, auth_style: AuthStyle) -> Self {
        Self {
            api_key,
            base_url,
            auth_style,
            client: Client::new(),
        }
    }
}
```

In `send_message`, replace the fixed header chain:

```rust
let response = self
    .client
    .post(format!("{}/v1/messages", self.base_url))
    .header("x-api-key", &self.api_key)
    .header("anthropic-version", "2023-06-01")
    .header("content-type", "application/json")
    .json(&request_body)
    .send()
    .await?;
```

with the branching version:

```rust
let request = self
    .client
    .post(format!("{}/v1/messages", self.base_url))
    .header("anthropic-version", "2023-06-01")
    .header("content-type", "application/json")
    .json(&request_body);

let request = match self.auth_style {
    AuthStyle::XApiKey => request.header("x-api-key", &self.api_key),
    AuthStyle::Bearer => request.bearer_auth(&self.api_key),
};

let response = request.send().await?;
```

- [ ] **Step 2: Update the single `create_provider` call site in `src/cli.rs`**

This is a binary-only crate, so `cargo test` needs the whole binary to compile. The cli.rs fix has to land in the same commit as the signature change.

Find the line in `src/cli.rs` (currently around line 28):

```rust
let provider = model::create_provider(&provider_name, api_key, base_url)?;
```

Replace with:

```rust
let auth_style = provider_config.auth_style;
let provider = model::create_provider(&provider_name, api_key, base_url, auth_style)?;
```

Note: `provider_config` is already bound on line 24 (`let provider_config = config.active_provider_config()?;`). `AuthStyle` is `Copy`, so no clone needed.

- [ ] **Step 3: Build the whole crate**

Run: `cargo build 2>&1 | tail -20`
Expected: clean build, no errors or warnings.

- [ ] **Step 4: Run the wiremock tests from Task 3**

Run: `cargo test claude_provider_sends -- --nocapture`
Expected: both pass.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: all tests pass — pre-existing tests plus the 2 new config tests from Task 1, the 2 new wiremock tests from Task 3, and the updated `test_parse_default_config` from Task 2.

- [ ] **Step 6: Commit (covers Tasks 3, 4, 5, and the cli.rs update)**

```bash
git add Cargo.toml src/model/mod.rs src/model/claude.rs src/cli.rs
git commit -m "model: thread AuthStyle through ClaudeProvider and create_provider

Routes minimax-anthropic to ClaudeProvider; branches on AuthStyle to
send either x-api-key or Authorization: Bearer. Verified offline with
wiremock tests for both header styles. Updates the single cli.rs call
site to forward auth_style from ProviderConfig."
```

---

## Task 6: Add the opt-in live integration test

**Files:**
- Modify: `src/model/claude.rs` (tests module)

- [ ] **Step 1: Add the env-gated live test**

Append to the `tests` module in `src/model/claude.rs`:

```rust
#[tokio::test]
async fn live_minimax_anthropic_roundtrip() {
    // Opt-in: this test hits a real network endpoint. Skip silently unless the
    // user explicitly asked for live tests via OH_MY_CODE_LIVE_TESTS=1.
    if std::env::var("OH_MY_CODE_LIVE_TESTS").ok().as_deref() != Some("1") {
        return;
    }

    let token = std::env::var("ANTHROPIC_AUTH_TOKEN")
        .expect("ANTHROPIC_AUTH_TOKEN must be set when OH_MY_CODE_LIVE_TESTS=1");
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.minimaxi.com/anthropic".to_string());
    let model = std::env::var("ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "MiniMax-M2.7-highspeed".to_string());

    let provider = ClaudeProvider::new(token, base_url, crate::config::AuthStyle::Bearer);

    let messages = vec![Message::user("Reply with exactly the word: pong")];
    let tools: Vec<ToolDef> = vec![];
    let config = ModelConfig {
        model_id: model,
        max_tokens: 64,
        temperature: 0.0,
    };

    let mut stream = provider
        .send_message(&messages, &tools, &config)
        .await
        .expect("provider send_message failed");

    let mut accumulated = String::new();
    let mut saw_delta = false;
    let mut saw_end = false;
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Delta { text } => {
                saw_delta = true;
                accumulated.push_str(&text);
            }
            StreamEvent::MessageEnd => {
                saw_end = true;
                break;
            }
            _ => {}
        }
    }

    assert!(saw_delta, "expected at least one Delta event");
    assert!(
        !accumulated.trim().is_empty(),
        "expected non-empty accumulated text"
    );
    assert!(saw_end, "expected MessageEnd event");
    eprintln!("live response: {:?}", accumulated);
}
```

- [ ] **Step 2: Verify the skip-by-default behavior**

Run: `cargo test live_minimax_anthropic`
Expected: PASS (test returns early because `OH_MY_CODE_LIVE_TESTS` is unset).

- [ ] **Step 3: Run the full suite one more time**

Run: `cargo test`
Expected: all tests pass, no network calls made.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -30`
Expected: no new warnings from the changes in this plan.

- [ ] **Step 5: Commit**

```bash
git add src/model/claude.rs
git commit -m "test: add opt-in live roundtrip test against minimax-anthropic

Gated on OH_MY_CODE_LIVE_TESTS=1; reads ANTHROPIC_AUTH_TOKEN,
ANTHROPIC_BASE_URL, and ANTHROPIC_MODEL from env with sensible
defaults for the MiniMax endpoint."
```

- [ ] **Step 6: Final manual live verification (optional but recommended)**

With a real token set in the environment:

```bash
OH_MY_CODE_LIVE_TESTS=1 \
ANTHROPIC_AUTH_TOKEN=<your-token> \
cargo test live_minimax_anthropic -- --nocapture
```

Expected: test passes, `live response: "..."` printed with MiniMax's reply.

---

## Verification summary

After all tasks complete, running `cargo test` (no env vars) should show:
- All pre-existing tests pass.
- New tests from Task 1 pass (`test_provider_config_auth_style_defaults_to_x_api_key`, `test_provider_config_auth_style_bearer`).
- Updated `test_parse_default_config` sees 5 providers and verifies `minimax-anthropic`.
- New tests from Task 3 pass offline via wiremock (`claude_provider_sends_x_api_key_header_by_default`, `claude_provider_sends_bearer_header_when_auth_style_bearer`).
- `live_minimax_anthropic_roundtrip` passes trivially (early return).

And `OH_MY_CODE_LIVE_TESTS=1 ANTHROPIC_AUTH_TOKEN=<key> cargo test live_minimax_anthropic -- --nocapture` should hit MiniMax's real endpoint and succeed.
