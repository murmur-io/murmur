use std::sync::Arc;

use crate::settings::AppConfig;
use crate::summarize::anthropic::AnthropicProvider;
use crate::summarize::claude_code::ClaudeCodeProvider;
use crate::summarize::ollama::OllamaProvider;
use crate::summarize::provider::SummarizerProvider;

pub mod anthropic;
pub mod claude_code;
pub mod ollama;
pub mod organize;
pub mod provider;
pub mod template;
pub mod timeline;

pub use provider::{Availability, MeetingMeta, SummarizeRequest, SummarizerProvider as _};

/// Default provider id when settings unset.
pub const DEFAULT_PROVIDER_ID: &str = "claude_code";

/// Stable provider ids (mirrors each provider's `id()`).
pub const PROVIDER_CLAUDE_CODE: &str = "claude_code";
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_OLLAMA: &str = "ollama";

/// Keychain account under which the Anthropic API key is stored
/// (matches `set_anthropic_key` / `has_anthropic_key` in `commands.rs`).
pub const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";

/// Build a provider by id, wiring config + secrets. Unknown id → `AppError::InvalidArg`.
pub fn make_provider(
    id: &str,
    config: &AppConfig,
) -> crate::error::Result<Arc<dyn SummarizerProvider>> {
    match id {
        PROVIDER_CLAUDE_CODE => Ok(Arc::new(ClaudeCodeProvider::with_binary(
            config.claude_binary.clone(),
        ))),
        PROVIDER_ANTHROPIC => {
            // Resolve the key from the Keychain here so providers never touch secrets.
            let api_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?;
            Ok(Arc::new(AnthropicProvider::new(
                api_key,
                config.anthropic_model.clone(),
            )))
        }
        PROVIDER_OLLAMA => Ok(Arc::new(OllamaProvider::new(
            config.ollama_base_url.clone(),
            config.ollama_model.clone(),
        ))),
        other => Err(crate::error::AppError::InvalidArg(format!(
            "unknown provider id: {other}"
        ))),
    }
}

/// All three provider instances (for availability fan-out in the Settings UI).
///
/// Best-effort: a failure to read the Anthropic key from the Keychain degrades to a
/// keyless `AnthropicProvider` (which then reports `Unavailable`) rather than failing the
/// whole fan-out.
pub fn all_providers(config: &AppConfig) -> Vec<Arc<dyn SummarizerProvider>> {
    let anthropic_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)
        .ok()
        .flatten();

    vec![
        Arc::new(ClaudeCodeProvider::with_binary(config.claude_binary.clone())),
        Arc::new(AnthropicProvider::new(
            anthropic_key,
            config.anthropic_model.clone(),
        )),
        Arc::new(OllamaProvider::new(
            config.ollama_base_url.clone(),
            config.ollama_model.clone(),
        )),
    ]
}
