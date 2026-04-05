# Env-driven quick-start provider

**Status:** Draft
**Date:** 2026-04-05

## Problem

Getting `oh-my-code` talking to a new backend today requires editing
`~/.config/oh-my-code/config.toml` and knowing which of five provider keys
(`claude`, `openai`, `zhipu`, `minimax`, `minimax-anthropic`) corresponds to
the endpoint you want to hit. A developer who just wants to paste an API key
and a URL has to learn the provider taxonomy first. Tools in this space
(aider, `llm`, claude-code) accept a "paste three things and go" path via env
vars — we don't.

We want that path as an additive convenience, without breaking the existing
`config.toml` workflow for users who have already invested in it.

## Scope

**In scope:**
- Three new env vars that, when all three are set, synthesize an in-memory
  provider entry and make it the active one: `API_KEY`, `BASE_URL`, `MODEL`.
- A small pure function that detects wire format + auth style from the URL.
- `AppConfig::load` integration: synthesize the virtual provider before
  returning, insert it into the providers map under the key `"env"`, set
  `default.provider = "env"` and `default.model = $MODEL`.
- Unit tests for the detection function (full truth table).
- Unit tests for `AppConfig::load` with synthetic env state (set vars, call
  load, assert the active provider reflects them; unset, assert TOML wins).
- Update `.env.example` to document the three vars as the primary quick-start
  path, keeping the existing per-provider secrets as the fallback for users
  who prefer TOML-driven setup.

**Out of scope:**
- Reworking the existing `config.toml` schema.
- Removing the five pre-existing provider entries (`claude`, `openai`,
  `zhipu`, `minimax`, `minimax-anthropic`) — they coexist.
- A `/provider` REPL command (future).
- Any change to `ClaudeProvider` or `OpenAIProvider` internals — the existing
  adapters are already correct for both wire formats.
- Supporting `API_KEY` + `BASE_URL` without `MODEL`. All three or none.
- Supporting per-provider env overrides (`ANTHROPIC_BASE_URL`, etc.) beyond
  the test-scoped ones already used by `live_minimax_anthropic_roundtrip`.

## Design

### 1. URL detection function

Pure function in a new module `src/config/detect.rs` (or inline in
`src/config.rs` — see file structure below):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DetectedBackend {
    /// The name we pass to `create_provider` to choose the wire format.
    pub provider_name: &'static str,
    pub auth_style: AuthStyle,
}

/// Decide wire format + auth style from a base URL.
///
/// Rules (first match wins):
///   1. Hostname is exactly `api.anthropic.com` → Anthropic wire, x-api-key
///   2. Path contains `/anthropic` (any case, any trailing slash) → Anthropic
///      wire, Bearer (third-party Anthropic-compatible endpoints)
///   3. Otherwise → OpenAI wire, Bearer
///
/// A malformed URL (no scheme, no host) falls through to rule 3 — the
/// resulting HTTP request will fail with a clear network error, which is a
/// better failure mode than refusing to start.
pub(crate) fn detect_backend(base_url: &str) -> DetectedBackend {
    let parsed = url::Url::parse(base_url).ok();

    if let Some(ref u) = parsed {
        if let Some(host) = u.host_str() {
            if host.eq_ignore_ascii_case("api.anthropic.com") {
                return DetectedBackend {
                    provider_name: "claude",
                    auth_style: AuthStyle::XApiKey,
                };
            }
        }
        if u.path().to_ascii_lowercase().contains("/anthropic") {
            return DetectedBackend {
                provider_name: "minimax-anthropic",
                auth_style: AuthStyle::Bearer,
            };
        }
    }

    DetectedBackend {
        provider_name: "openai",
        auth_style: AuthStyle::Bearer,
    }
}
```

Notes:
- We reuse the existing provider keys (`"claude"`, `"minimax-anthropic"`,
  `"openai"`) as the `provider_name` return value. `create_provider` already
  routes these to the right adapter — no new routing needed.
- `url::Url` is already a transitive dependency via `reqwest`, but we should
  add it to `Cargo.toml` explicitly so the dependency relationship is
  declared. It's ~100KB of source, already compiled into the build.
- Case-insensitive on hostname and path: `API.Anthropic.COM` and
  `/Anthropic/` both resolve correctly.
- Malformed URL does NOT panic or error — we fall through to OpenAI-compat.
  The subsequent HTTP request will fail cleanly with a network error
  reported through the existing `send_message` error path.

### 2. `AppConfig::load` integration

After loading `config.toml`, before returning:

```rust
if let Some((api_key_env, base_url, model)) = read_env_quick_start() {
    let backend = detect_backend(&base_url);
    cfg.providers.insert(
        "env".to_string(),
        ProviderConfig {
            api_key_env, // = "API_KEY"
            base_url,
            auth_style: backend.auth_style,
        },
    );
    cfg.default.provider = "env".to_string();
    cfg.default.model = model;
}
```

`read_env_quick_start()` returns `Some` only when **all three** of
`API_KEY`, `BASE_URL`, `MODEL` are set and non-empty. If only one or two
are set, it returns `None` and the function also emits a `warn!`-level
message on stderr (via `eprintln!` since there's no logging crate in the
project today) explaining which vars are missing — this avoids silent
"I set API_KEY and nothing happens" debugging.

The provider name we inject into `create_provider` is
`backend.provider_name` (`"claude"`, `"minimax-anthropic"`, or `"openai"`),
**not** `"env"`. The `"env"` key is purely the configuration-map label;
`create_provider` still receives the real wire-format name.

This requires a small refactor to how `cli.rs` calls `create_provider`:
today it passes `config.default.provider.clone()` as the provider name.
Under the new design, it should pass the *routing* name (what wire format
to use), which may differ from the *config-map key*. The cleanest
resolution: when `default.provider == "env"`, look up what `detect_backend`
returned. Two ways:

- **(a) Store the routing name on `ProviderConfig`** as an optional field
  `routing_name: Option<String>`. If set, `create_provider` uses it;
  otherwise the config-map key IS the routing name (backward compat).
- **(b) Store the `DetectedBackend` result on `AppConfig`** itself.

Option (a) is cleaner and more self-contained. `ProviderConfig` gains one
optional serde-skipped field (`#[serde(skip)]` so it never appears in TOML).
`cli.rs` becomes:

```rust
let provider_name = provider_config
    .routing_name
    .clone()
    .unwrap_or_else(|| config.default.provider.clone());
```

### 3. `.env.example` update

Add a prominent "Quick start" section at the top, keep the existing
per-provider secrets section as the fallback:

```bash
# =============================================================================
# .env.example
#
# Two ways to configure oh-my-code:
#
#   Quick start (three vars, no config.toml editing):
#     Set API_KEY + BASE_URL + MODEL below.
#     oh-my-code auto-detects Anthropic vs OpenAI wire format from BASE_URL.
#
#   Multi-provider (more control):
#     Leave API_KEY/BASE_URL/MODEL empty and set the per-vendor key vars
#     at the bottom. Select the active provider in
#     ~/.config/oh-my-code/config.toml under [default].
#
# Precedence: if all three of API_KEY, BASE_URL, and MODEL are set, they win
# over config.toml. If any is unset, config.toml drives provider selection.
# =============================================================================

# ---- Quick start ------------------------------------------------------------

API_KEY=
BASE_URL=
MODEL=

# Examples you can paste into the three above:
#
#   Anthropic (real):
#     API_KEY=sk-ant-...
#     BASE_URL=https://api.anthropic.com
#     MODEL=claude-sonnet-4-5
#
#   MiniMax Anthropic-compatible:
#     API_KEY=sk-cp-...
#     BASE_URL=https://api.minimaxi.com/anthropic
#     MODEL=MiniMax-M2.7-highspeed
#
#   OpenAI:
#     API_KEY=sk-...
#     BASE_URL=https://api.openai.com
#     MODEL=gpt-4o

# ---- Multi-provider fallback ------------------------------------------------
# (existing per-vendor key vars preserved)

ANTHROPIC_API_KEY=
ANTHROPIC_AUTH_TOKEN=
OPENAI_API_KEY=
ZHIPU_API_KEY=
MINIMAX_API_KEY=
```

### 4. Test strategy

**Pure unit tests for `detect_backend`** — no I/O, no mocking, ~12 cases:

| URL | Expected |
|---|---|
| `https://api.anthropic.com` | claude + XApiKey |
| `https://api.anthropic.com/v1/messages` | claude + XApiKey |
| `https://API.Anthropic.COM` (case) | claude + XApiKey |
| `https://api.minimaxi.com/anthropic` | minimax-anthropic + Bearer |
| `https://api.minimaxi.com/anthropic/` (trailing slash) | minimax-anthropic + Bearer |
| `https://api.minimaxi.com/ANTHROPIC/v1/messages` (case + path tail) | minimax-anthropic + Bearer |
| `https://corp-proxy.internal/anthropic/v1` | minimax-anthropic + Bearer |
| `https://api.openai.com` | openai + Bearer |
| `https://api.openai.com/v1/chat/completions` | openai + Bearer |
| `https://api.zhipu.com/v4` | openai + Bearer |
| `not a url` (malformed) | openai + Bearer (fallback) |
| `` (empty) | openai + Bearer (fallback) |

**`AppConfig::load` tests** with env fiddling — need to be careful about
test isolation because env vars are process-global. Use a serial-test
approach: one `#[test]` that sets, loads, asserts, unsets, in that order.
Alternatively use the `serial_test` crate; but we can avoid a new dep by
using a mutex in the test module. Given 97 existing tests already handle
various scenarios, and these are the first env-mutating tests in the
codebase, a simple `once_cell::sync::Mutex` (or `std::sync::Mutex`) guard
around the test body is enough.

Cases to cover:
1. All three env vars set → active provider is the synthesized one, with
   correctly detected auth style.
2. Only two of three set → env path inactive, TOML wins, stderr warning
   emitted (check a flag or captured output).
3. None set → today's behavior, no change.
4. All three set with Anthropic URL → XApiKey auth.
5. All three set with MiniMax Anthropic URL → Bearer + claude wire.
6. All three set with OpenAI URL → Bearer + openai wire.

### 5. File structure

- Modify `src/config.rs`: add `AuthStyle` usage is already there; add
  `routing_name` field on `ProviderConfig` (serde-skipped); add private
  `detect_backend` function and `read_env_quick_start` helper; extend
  `AppConfig::load` with the synthesis step.
- Modify `src/model/mod.rs`: no change to `create_provider` itself, but
  clarify via a comment that it dispatches on wire-format name, which may
  come from `ProviderConfig.routing_name` rather than the map key.
- Modify `src/cli.rs`: use `provider_config.routing_name.unwrap_or(...)`
  when calling `create_provider`.
- Modify `.env.example`: restructure per section 3 above.
- Add `url = "2"` to `Cargo.toml` dependencies (already transitive via
  reqwest; declaring explicitly is good hygiene).

## Risks and open questions

- **Env-var test isolation.** First time we have tests that mutate env
  vars. Must serialize them or tests will race under `cargo test`'s
  default parallelism. Plan: single combined test function that runs all
  scenarios sequentially, with cleanup between them. Cheap, no new deps.
- **Stderr warning without a log crate.** Using `eprintln!` for the
  "two of three set" warning is ugly but consistent with the rest of the
  codebase (no `tracing`/`log` today). If a log crate is added later this
  one line is trivial to migrate.
- **`routing_name` field adds a wart to `ProviderConfig`.** It's
  serde-skipped so users never see it in TOML, but future readers of the
  struct will wonder about it. The doc comment on the field should be
  explicit: "populated only for env-quick-start synthesized providers".
- **`create_provider` handles `"env"` gracefully** only via the routing
  indirection. If anyone ever calls `create_provider("env", ...)`
  directly, it will fail with "unsupported provider: env". This is fine
  because the only call site (`cli.rs`) uses the indirection.
- **Test token exposure risk.** The unit tests use literal fake strings
  like `"sk-test-quick-start"` — no real credentials. `.env.example`
  shows abbreviated example values, never a real key.
