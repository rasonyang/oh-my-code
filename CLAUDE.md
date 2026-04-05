# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`oh-my-code` is a Rust-native interactive terminal coding assistant — a rebuild of the Claude Code experience that supports multiple providers (Claude, OpenAI/GPT-compatible, Zhipu, MiniMax) and ships its own native search toolchain instead of shelling out to `rg`/`fd`.

## Commands

```bash
cargo build                         # debug build
cargo build --release               # release binary at target/release/oh-my-code
cargo test                          # run full test suite (expect ~97 tests)
cargo test <module::path>           # run a subset, e.g. cargo test model::types
cargo test <name> -- --nocapture    # show println! output from a single test
cargo check                         # fast type-check without codegen
cargo clippy --all-targets          # lint
cargo fmt                           # format
```

Note: this crate is a binary-only crate (no `lib` target). `cargo test --lib ...` will fail — use `cargo test <path>` instead.

Running the REPL requires an API key in the env var the chosen provider expects (see `src/config.rs::default_config` for the mapping, e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `ZHIPU_API_KEY`, `MINIMAX_API_KEY`):

```bash
MINIMAX_API_KEY=<key> ./target/release/oh-my-code
```

On first run, a default config is written to `~/.config/oh-my-code/config.toml`. Sessions are persisted as JSON under `~/.config/oh-my-code/sessions/`.

## Architecture

The system is an async (tokio) agent loop that streams responses from a pluggable provider and dispatches tool calls back to a local registry.

**Entry flow:** `main.rs` → loads `AppConfig` → constructs `cli::Repl` → rustyline loop reads input → `Agent::run_turn` is called per user message. Slash commands (`/quit`, `/clear`, `/model`, `/session`, `/help`) are handled directly in `cli.rs` before falling through to the agent.

**Provider abstraction (`src/model/`):** The `Provider` trait returns a `BoxStream<'static, StreamEvent>` so every backend can produce a uniform stream of `Delta`, `ToolUseStart`, `ToolUseDelta`, `InputJsonComplete`, `MessageEnd` events. `create_provider` dispatches by name: `"claude"` and `"minimax-anthropic"` → `claude.rs` (Anthropic Messages API SSE; `minimax-anthropic` targets MiniMax's Anthropic-compatible endpoint with `AuthStyle::Bearer` instead of `x-api-key`), and `"openai" | "zhipu" | "minimax"` → `openai.rs` (OpenAI-compatible Chat Completions SSE). Auth header selection is driven by `ProviderConfig.auth_style` (`AuthStyle::XApiKey` default, `AuthStyle::Bearer` for Anthropic-compatible third parties). Internal `Message` / `ContentBlock` types in `model/types.rs` are the canonical format; each adapter translates to/from its wire format.

**Agent loop (`src/agent/mod.rs`):** `Agent::run_turn` pushes the user message, then loops up to `max_turns` times: build system prompt → send history + tools to provider → consume the stream, accumulating text and tool_use blocks → append assistant message → if no tool calls, return; otherwise execute tools via `ToolExecutor` and append a `tool_results` message before looping. The system prompt is rebuilt each iteration so the current Plan/Act mode label is fresh.

**Tool execution (`src/agent/executor.rs`):** `ToolExecutor::execute_batch` partitions calls by `Tool::is_read_only()`. Read-only tools run **concurrently** via `join_all`; write tools run **sequentially** in arrival order. When `plan_mode` is true, writes are short-circuited to an error before invocation. Unknown tool names become error results. Output order matches the input order regardless of partitioning.

**Plan/Act mode (`src/agent/plan.rs`):** `PlanModeState` wraps `Arc<AtomicBool>`. It's cloned into the `EnterPlanMode` / `ExitPlanMode` tool instances at registration time, so an LLM-invoked mode switch mutates the same state the `Agent` and `ToolExecutor` read. Mode is reflected in the system prompt on each turn.

**Tools (`src/tool/`):** Each tool implements the `Tool` trait (`name`, `description`, `input_schema`, `is_read_only`, `async execute`). `create_default_registry(plan_state)` wires all 11 bundled tools: `think`, `grep`, `glob`, `file_read`, `file_edit`, `file_write`, `bash`, `enter_plan_mode`, `exit_plan_mode`, `web_fetch`, `web_search`. When adding a tool, register it here and decide carefully whether it is read-only (affects concurrency + plan-mode gating).

**Native search (`src/search/`):** Instead of spawning `rg`/`fd`, search uses `grep-searcher` + `grep-regex` (`ripgrep.rs`), the `ignore` walker with `glob` patterns (`finder.rs`), and `syntect` for ANSI-highlighted output (`highlight.rs`). The `grep` and `glob` tools delegate here. Ignore patterns come from `AppConfig.search.ignore_patterns`.

**Config (`src/config.rs`):** TOML-based `AppConfig` with `[default]`, `[providers.*]`, `[search]`, `[session]` sections. `resolve_api_key()` reads the env var named in the active provider's `api_key_env`. `resolved_session_dir()` expands a leading `~/`. `config/default.toml` in the repo is the template baked into tests via `include_str!`.

**Sessions (`src/session/`):** `SessionData` is a JSON-serializable snapshot keyed by UUID. `storage.rs` handles load/save under the resolved session directory. The REPL exposes this via `/session` subcommands.

## Conventions worth knowing

- Every tool's `is_read_only()` return value is load-bearing — it drives both concurrency and plan-mode enforcement. Double-check it when adding or modifying a tool.
- The agent streams assistant text to stdout as it arrives (`print!` + flush) while also accumulating it into history. Don't rework the stream consumer without preserving both behaviors.
- Message translation between internal types and provider wire formats lives in small helper fns inside each provider adapter (`to_claude_message`, `to_openai_messages`, etc.) — keep new fields flowing through both sides.
- New providers should be added as a variant in `create_provider` and either reuse `openai.rs` (if OpenAI-compatible) or get their own adapter module.
