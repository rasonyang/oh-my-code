# MiniMax-via-Anthropic provider + live integration test

**Status:** Draft
**Date:** 2026-04-05

## Problem

MiniMax exposes a Claude-compatible endpoint at `https://api.minimaxi.com/anthropic`
that speaks the Anthropic Messages wire format, but authenticates via
`Authorization: Bearer <token>` (the `ANTHROPIC_AUTH_TOKEN` convention) instead of
Anthropic's `x-api-key` header. Today `oh-my-code`'s `ClaudeProvider` hardcodes
`x-api-key`, and there is no provider entry pointing at MiniMax's Anthropic
endpoint. We want to:

1. Reach MiniMax's M2.7 model through the existing Claude adapter (reusing SSE +
   tool-translation code) rather than the OpenAI-compatible adapter.
2. Have a live integration test that exercises the full Claude provider
   round-trip against a real server — without regressing `cargo test`.

## Non-goals

- Replacing the existing `minimax` entry (which uses the OpenAI-compatible
  adapter at `https://api.minimax.chat/v1`). Both will coexist.
- Generic custom-header support. The auth selector is a closed enum, not a
  free-form map.
- Wiring `API_TIMEOUT_MS` or `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`. Those
  are official-Claude-Code harness concerns that don't apply here.
- Changing the default provider. `claude` stays default; the new entry is
  opt-in.

## Design

### 1. `ProviderConfig` gains an `auth_style` field

`src/config.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    #[default]
    XApiKey,
    Bearer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
    pub base_url: String,
    #[serde(default)]
    pub auth_style: AuthStyle,
}
```

`#[serde(default)]` means existing TOML files (including every test fixture)
continue to parse unchanged — the default is `XApiKey`, matching current
behavior. The enum is closed and explicit, avoiding typos that a free-form
string would invite.

### 2. New provider entry: `minimax-anthropic`

Added to both `src/config.rs::default_config` and `config/default.toml`:

```toml
[providers.minimax-anthropic]
api_key_env = "ANTHROPIC_AUTH_TOKEN"
base_url = "https://api.minimaxi.com/anthropic"
auth_style = "bearer"
```

The key `minimax-anthropic` distinguishes it from the existing `minimax` entry
(which remains as the OpenAI-compatible route). Users opt in by editing their
`~/.config/oh-my-code/config.toml` `default.provider` or via `/model`.

### 3. `create_provider` routing

`src/model/mod.rs`:

```rust
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

`AuthStyle` is re-exported from `config` (or moved to a shared module) so `model`
can depend on it without a circular import.

### 4. `ClaudeProvider` branches on auth style

`src/model/claude.rs`:

```rust
pub struct ClaudeProvider {
    api_key: String,
    base_url: String,
    auth_style: AuthStyle,
    client: Client,
}

// in send_message, replace the fixed x-api-key header with:
let request = self
    .client
    .post(format!("{}/v1/messages", self.base_url))
    .header("anthropic-version", "2023-06-01")
    .header("content-type", "application/json")
    .json(&request_body);

let request = match self.auth_style {
    AuthStyle::XApiKey => request.header("x-api-key", &self.api_key),
    AuthStyle::Bearer  => request.bearer_auth(&self.api_key),
};

let response = request.send().await?;
```

Everything else — wire types, `extract_system_prompt`, `to_claude_message`,
`parse_sse_line`, tool translation — is untouched. MiniMax claims Anthropic
wire-format compatibility, so the rest of the adapter is reused verbatim.

### 5. Call-site updates

Every place that calls `create_provider` must now pass `auth_style` from the
active `ProviderConfig`. Based on the current codebase, this is primarily
`src/cli.rs` / `src/agent/mod.rs` construction paths. They already read
`config.active_provider_config()` — they just need to forward
`provider.auth_style`.

### 6. Live integration test (inline, env-gated)

In the existing `#[cfg(test)] mod tests` block of `src/model/claude.rs`:

```rust
#[tokio::test]
async fn live_minimax_anthropic_roundtrip() {
    // Opt-in gate: this test hits a real network endpoint.
    if std::env::var("OH_MY_CODE_LIVE_TESTS").ok().as_deref() != Some("1") {
        return;
    }

    let token = std::env::var("ANTHROPIC_AUTH_TOKEN")
        .expect("ANTHROPIC_AUTH_TOKEN must be set when OH_MY_CODE_LIVE_TESTS=1");
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.minimaxi.com/anthropic".to_string());
    let model = std::env::var("ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "MiniMax-M2.7-highspeed".to_string());

    let provider = ClaudeProvider::new(token, base_url, AuthStyle::Bearer);

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
    assert!(!accumulated.trim().is_empty(), "expected non-empty text");
    assert!(saw_end, "expected MessageEnd event");
    eprintln!("live response: {:?}", accumulated);
}
```

**Key properties:**
- **Skip-by-default**: `cargo test` without `OH_MY_CODE_LIVE_TESTS=1` returns
  immediately. The test appears as passing — acceptable because the assertion
  we care about (wire compatibility) can only be verified with a live endpoint,
  and the alternative (mocking) would tautologically test our own code.
- **No secrets committed**: token is read from env at runtime.
- **Asserts wire-level success, not content**: we check that SSE events flowed
  and a final `MessageEnd` arrived — not that the model said "pong". Content
  assertions would be flaky across model versions.
- **Temperature 0** to reduce variance, though we don't depend on exact output.

Running the live test:
```bash
OH_MY_CODE_LIVE_TESTS=1 \
ANTHROPIC_AUTH_TOKEN=<token> \
cargo test live_minimax_anthropic -- --nocapture
```

## Files changed

| File | Change |
| --- | --- |
| `src/config.rs` | Add `AuthStyle` enum, field on `ProviderConfig`, new provider in `default_config`, update `providers.len()` assertion to 5 |
| `config/default.toml` | Add `[providers.minimax-anthropic]` section |
| `src/model/mod.rs` | Import `AuthStyle`, extend `create_provider` signature, route `minimax-anthropic` to `ClaudeProvider` |
| `src/model/claude.rs` | `ClaudeProvider` stores `AuthStyle`; `send_message` branches on header style; add `live_minimax_anthropic_roundtrip` test |
| Call sites of `create_provider` | Forward `auth_style` from `ProviderConfig` |

## Test plan

- **`cargo test`** (no env vars): all existing ~92 tests pass; new live test
  skips silently; `test_parse_default_config` updated to expect 5 providers.
- **`cargo build --release`**: compiles clean.
- **`cargo clippy --all-targets`**: no new warnings.
- **`OH_MY_CODE_LIVE_TESTS=1 ANTHROPIC_AUTH_TOKEN=<key> cargo test live_minimax_anthropic -- --nocapture`**:
  hits real MiniMax endpoint, prints streamed text, all assertions pass.
- **Manual REPL smoke** (optional, after implementation):
  set `default.provider = "minimax-anthropic"` in config, run the binary with
  `ANTHROPIC_AUTH_TOKEN` set, send a prompt, confirm streamed reply.

## Risks and open questions

- **MiniMax wire divergence**: if MiniMax's `/anthropic` endpoint differs from
  Anthropic in ways beyond auth (e.g., missing SSE event types, different
  tool-use schema), the live test will fail and we'll need a compatibility
  layer. The test is precisely how we find this out — that's the point.
- **Tool-use behavior against MiniMax**: this spec's live test sends no tools.
  A follow-up could add a second live test that requests a `bash`-tool call and
  verifies `ToolUseStart` + `InputJsonComplete` events. Out of scope for this
  change; noted for later.
- **`ClaudeProvider::name()` still returns `"claude"`** even when constructed
  for `minimax-anthropic`. This is cosmetic (used only for logging) and keeping
  it unchanged minimizes blast radius. If logs become confusing we can revisit.
