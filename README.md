# oh-my-code

A Rust-native interactive terminal coding assistant. Rebuilds the Claude Code experience in Rust with a pluggable provider layer, a native search toolchain, and a concurrent tool executor with read/write safety partitioning.

## Features

- **Multi-provider**: Claude (Anthropic Messages API), OpenAI / GPT, Zhipu, MiniMax (OpenAI-compatible endpoint), and MiniMax (Anthropic-compatible endpoint at `api.minimaxi.com/anthropic` with `Bearer` auth). Add a new provider by extending `create_provider` in `src/model/mod.rs`.
- **Two configuration paths**: a three-variable `.env` quick start (`API_KEY` + `BASE_URL` + `MODEL`, with wire format and auth style auto-detected from the URL) that coexists with a full `config.toml` multi-provider setup for users who want to juggle several backends.
- **Streaming agent loop**: Assistant text streams to the terminal as it arrives; tool calls are dispatched and fed back into the conversation until the model stops calling tools.
- **11 built-in tools**: `think`, `grep`, `glob`, `file_read`, `file_edit`, `file_write`, `bash`, `enter_plan_mode`, `exit_plan_mode`, `web_fetch`, `web_search`.
- **Concurrent tool execution with safety partitioning**: Read-only tools run in parallel via `join_all`; write tools run sequentially in arrival order.
- **Plan / Act modes**: An atomic shared flag gates writes. In Plan mode, the model can read and reason but cannot mutate files or run shell commands.
- **Native search toolchain**: Uses `grep-searcher` + `grep-regex` + `ignore` + `syntect` directly — no shelling out to `rg` or `fd`.
- **Interactive REPL**: `rustyline`-based line editor with slash commands (`/help`, `/model`, `/session`, `/clear`, `/quit`).
- **File-based sessions**: Conversations are persisted as JSON under `~/.config/oh-my-code/sessions/`.

## Install

Requires a recent stable Rust toolchain (install via [rustup](https://rustup.rs)).

```bash
git clone <this-repo>
cd oh-my-code
cargo build --release
```

The binary lands at `target/release/oh-my-code`.

## Configuration

There are two ways to configure `oh-my-code`. They coexist — the quick-start env vars win when all three are set; otherwise `config.toml` drives provider selection.

### Quick start (`.env`, three variables)

```bash
cp .env.example .env
# then edit .env and fill in:
#   API_KEY=sk-...
#   BASE_URL=https://api.anthropic.com          # or any OpenAI / Anthropic-compatible endpoint
#   MODEL=claude-sonnet-4-5
```

`oh-my-code` reads `.env` at startup via `dotenvy`, then auto-detects the wire format and auth header from `BASE_URL`:

| URL pattern                  | Wire format             | Auth header               |
|------------------------------|-------------------------|---------------------------|
| host is `api.anthropic.com`  | Anthropic Messages API  | `x-api-key`               |
| path contains `/anthropic`   | Anthropic-compatible    | `Authorization: Bearer`   |
| anything else                | OpenAI-compatible       | `Authorization: Bearer`   |

If any of the three vars is missing or empty, the quick-start path stays inactive and `config.toml` is used instead. Partial configuration (e.g. `API_KEY` and `BASE_URL` set but `MODEL` missing) prints a warning to stderr so you know why your overrides are not taking effect.

### Multi-provider via `config.toml`

On first run, a default config is written to `~/.config/oh-my-code/config.toml`. Edit it to change the active provider, model, search ignore patterns, or session storage directory. The in-repo template lives at `config/default.toml`. Each provider reads its API key from the env var named in its `api_key_env` field:

| Provider            | Env var                | Endpoint                                 | Auth          |
|---------------------|------------------------|------------------------------------------|---------------|
| `claude`            | `ANTHROPIC_API_KEY`    | `https://api.anthropic.com`              | `x-api-key`   |
| `minimax-anthropic` | `ANTHROPIC_AUTH_TOKEN` | `https://api.minimaxi.com/anthropic`     | `Bearer`      |
| `openai`            | `OPENAI_API_KEY`       | `https://api.openai.com`                 | `Bearer`      |
| `zhipu`             | `ZHIPU_API_KEY`        | `https://open.bigmodel.cn/api/paas/v4`   | `Bearer`      |
| `minimax`           | `MINIMAX_API_KEY`      | `https://api.minimax.chat/v1`            | `Bearer`      |

## Usage

```bash
# Option 1 — quick start (reads .env)
./target/release/oh-my-code

# Option 2 — inline env var, provider chosen in config.toml
ANTHROPIC_API_KEY=<key> ./target/release/oh-my-code
```

Once in the REPL, type a request in natural language. Useful slash commands:

- `/help` — list commands
- `/model` — switch model
- `/session` — list / load / save sessions
- `/clear` — clear the current conversation history
- `/quit` — exit

## Development

```bash
cargo test              # full test suite
cargo test <path>       # run a subset, e.g. cargo test model::types
cargo clippy --all-targets
cargo fmt
```

Note: `oh-my-code` is a binary-only crate, so `cargo test --lib ...` will fail — use `cargo test <module_path>` instead.

See [`CLAUDE.md`](CLAUDE.md) for a deeper architectural tour.

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE) for the full text.
