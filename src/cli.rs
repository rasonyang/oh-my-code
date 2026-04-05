use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::agent::Agent;
use crate::config::AppConfig;
use crate::model::{self, Provider};
use crate::model::types::ModelConfig;
use crate::session;
use crate::session::storage;
use crate::tool::{self, ToolRegistry};

pub struct Repl {
    agent: Agent,
    provider: Box<dyn Provider>,
    registry: ToolRegistry,
    config: AppConfig,
    session: session::Session,
}

impl Repl {
    pub fn new(config: AppConfig) -> Result<Self> {
        let api_key = config.resolve_api_key()?;
        let provider_config = config.active_provider_config()?;
        // When the synthetic "env" provider is active, routing_name carries the
        // real wire-format name ("claude" / "minimax-anthropic" / "openai"). For
        // TOML-loaded providers routing_name is None and the config-map key IS the
        // routing name.
        let provider_name = provider_config
            .routing_name
            .clone()
            .unwrap_or_else(|| config.default.provider.clone());
        let base_url = provider_config.base_url.clone();

        let auth_style = provider_config.auth_style;
        let provider = model::create_provider(&provider_name, api_key, base_url, auth_style)?;

        let model_config = ModelConfig {
            model_id: config.default.model.clone(),
            max_tokens: 8192,
            temperature: 0.0,
        };

        let agent = Agent::new(model_config);
        let plan_state = agent.plan_mode_state();
        let registry = tool::create_default_registry(plan_state);

        let session_dir = config.resolved_session_dir();
        let session = session::Session::new(&config.default.provider, &config.default.model, session_dir);

        Ok(Self {
            agent,
            provider,
            registry,
            config,
            session,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let version = env!("CARGO_PKG_VERSION");
        let provider_name = &self.config.default.provider;
        let model_id = &self.agent.model_config.model_id;

        println!("oh-my-code v{} | provider: {} | model: {}", version, provider_name, model_id);
        println!("Type /help for available commands. Ctrl+D to exit.");
        println!();

        let mut rl = DefaultEditor::new()?;

        let history_path = dirs::config_dir()
            .map(|p| p.join("oh-my-code").join("history.txt"));

        if let Some(ref path) = history_path {
            // Ignore error if history file doesn't exist yet
            let _ = rl.load_history(path);
        }

        loop {
            let prompt = if self.agent.plan_mode() {
                "oh-my-code [PLAN]> ".to_string()
            } else {
                "oh-my-code> ".to_string()
            };

            match rl.readline(&prompt) {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    let _ = rl.add_history_entry(&line);

                    if line.starts_with('/') {
                        if self.handle_command(&line) {
                            break;
                        }
                    } else {
                        if let Err(e) = self.agent.run_turn(&line, self.provider.as_ref(), &self.registry).await {
                            eprintln!("Error: {}", e);
                            break;
                        }
                        self.session.update_messages(self.agent.messages());
                        if let Err(e) = self.session.save() {
                            eprintln!("[warning] Failed to save session: {}", e);
                        }
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    println!("Goodbye!");
                    break;
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }

        if let Some(ref path) = history_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = rl.save_history(path);
        }

        Ok(())
    }

    fn handle_command(&mut self, line: &str) -> bool {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        let cmd = parts[0];
        let args = parts.get(1).map(|s| s.trim());

        match cmd {
            "/quit" | "/exit" => {
                println!("Goodbye!");
                true
            }
            "/clear" => {
                self.agent.clear_history();
                println!("Conversation history cleared.");
                false
            }
            "/model" => {
                match args {
                    None | Some("") => {
                        println!("Current model: {}", self.agent.model_config.model_id);
                    }
                    Some(new_model) => {
                        let new_config = ModelConfig {
                            model_id: new_model.to_string(),
                            max_tokens: self.agent.model_config.max_tokens,
                            temperature: self.agent.model_config.temperature,
                        };
                        self.agent.set_model_config(new_config);
                        println!("Model set to: {}", new_model);
                    }
                }
                false
            }
            "/session" => {
                let sub_parts: Vec<&str> = args
                    .unwrap_or("")
                    .splitn(2, ' ')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                let subcmd = sub_parts.first().copied().unwrap_or("");
                let sub_args = sub_parts.get(1).copied().unwrap_or("");

                match subcmd {
                    "list" => {
                        match storage::list_sessions(&self.session.storage_dir) {
                            Ok(summaries) => {
                                if summaries.is_empty() {
                                    println!("No saved sessions.");
                                } else {
                                    println!("{:<10} {:<30} {:>6} {}", "ID", "Model", "Msgs", "Updated");
                                    println!("{}", "-".repeat(60));
                                    for s in &summaries {
                                        let short_id = if s.id.len() >= 8 { &s.id[..8] } else { &s.id };
                                        println!("{:<10} {:<30} {:>6} {}", short_id, s.model, s.message_count, s.updated_at);
                                    }
                                }
                            }
                            Err(e) => eprintln!("Error listing sessions: {}", e),
                        }
                    }
                    "new" => {
                        self.agent.clear_history();
                        self.session = session::Session::new(
                            &self.config.default.provider,
                            &self.config.default.model,
                            self.config.resolved_session_dir(),
                        );
                        println!("New session started (id: {}).", &self.session.data.id[..8]);
                    }
                    "load" => {
                        if sub_args.is_empty() {
                            println!("Usage: /session load <id-prefix>");
                        } else {
                            let prefix = sub_args;
                            match storage::list_sessions(&self.session.storage_dir) {
                                Ok(summaries) => {
                                    let found = summaries.iter().find(|s| s.id.starts_with(prefix));
                                    match found {
                                        None => println!("No session found with id prefix '{}'.", prefix),
                                        Some(summary) => {
                                            let full_id = summary.id.clone();
                                            match storage::load_session(&self.session.storage_dir, &full_id) {
                                                Ok(data) => {
                                                    let messages = data.messages.clone();
                                                    self.agent.clear_history();
                                                    for msg in &messages {
                                                        self.agent.messages.push(msg.clone());
                                                    }
                                                    self.session = session::Session::from_data(
                                                        data,
                                                        self.config.resolved_session_dir(),
                                                    );
                                                    println!("Loaded session {} ({} messages).", &full_id[..8], messages.len());
                                                }
                                                Err(e) => eprintln!("Error loading session: {}", e),
                                            }
                                        }
                                    }
                                }
                                Err(e) => eprintln!("Error listing sessions: {}", e),
                            }
                        }
                    }
                    _ => {
                        println!("Usage:");
                        println!("  /session list          List all saved sessions");
                        println!("  /session new           Start a new session");
                        println!("  /session load <id>     Load a session by id prefix");
                    }
                }
                false
            }
            "/help" => {
                println!("Available commands:");
                println!("  /help              Show this help message");
                println!("  /clear             Clear conversation history");
                println!("  /model [name]      Show or set the current model");
                println!("  /session list      List all saved sessions");
                println!("  /session new       Start a new session");
                println!("  /session load <id> Load a session by id prefix");
                println!("  /quit, /exit       Exit oh-my-code");
                false
            }
            unknown => {
                println!("Unknown command: {}", unknown);
                false
            }
        }
    }
}
