use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::summarize::meta::CallMeta;

/// One entry in a bundled model **hint** list.
///
/// The distinction from the old `&[&str]` constants is the whole point of this type: these lists
/// are a convenience so the picker has something to show, **never an allowlist**. A model id the
/// user types that is absent here is a custom id, not corruption — nothing may clear it, reject it
/// or "repair" it. The FE learns which lists are merely bundled (versus fetched from a live
/// endpoint) from `ModelOptionDto::source`, so it can offer Refresh only where refreshing means
/// something and can say plainly that a bundled list may be out of date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudModel {
    /// The id passed verbatim to the CLI / API.
    pub id: &'static str,
    /// Human display name. The picker shows this, not the raw id.
    pub label: &'static str,
    /// One-clause hint about when to reach for it.
    pub note: &'static str,
}

/// Length ceiling for an id that becomes a CLI argument. Real CLI model ids are short slugs.
const MODEL_ID_MAX_CHARS: usize = 64;

/// Storage bound for an id that is only ever sent in a JSON body.
///
/// A BOUND, not a shape rule. Ollama's live catalog returns Hugging Face paths such as
/// `hf.co/TheBloke/Mistral-7B-Instruct-v0.2-GGUF:Q4_K_M`, comfortably past 64 characters, and no
/// observed id approaches this figure — it exists only so a config field cannot grow without limit
/// (it is stored in SQLite and echoed in the egress ledger).
const CATALOG_MODEL_ID_MAX_CHARS: usize = 512;

/// Characters legal in any model id. `@` and `/` appear in real Hugging Face and vendor-scoped ids.
fn model_id_shape_is_safe(model: &str, max_chars: usize) -> bool {
    // A leading `-` is the argv-injection shape; a `..`/`.` segment is the path-traversal shape.
    // Both are refused everywhere, because neither is a real model id on any arm.
    !model.is_empty()
        && model.len() <= max_chars
        && !model.starts_with('-')
        && !model.split('/').any(|segment| segment == ".." || segment == ".")
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/' | '@'))
}

/// Accept an id destined for a JSON REQUEST BODY (`anthropic`, `ollama`, `gateway`).
///
/// Accept an id destined for a JSON REQUEST BODY (`anthropic`, `ollama`, `gateway`).
///
/// NO CHARACTER ALLOWLIST here, and a storage-sized ceiling rather than a slug-sized one.
///
/// This used to share [`model_id_shape_is_safe`] with the CLI arms, differing on length alone.
/// On these arms the id is `serde_json`-escaped into a request body; it never becomes argv, a URL
/// path or a filename (checked: `ollama.rs` and `gateway.rs` put it in the body, plus a purely
/// internal recovery key). So an ASCII allowlist and a 200-character cap prevented nothing and
/// could only REFUSE legitimate ids — `vendor/model+preview`, any non-ASCII id a future endpoint
/// returns — at save, and worse, `AppConfig::load` then CLEARED such an id on every launch. That
/// is catalog-as-allowlist behaviour wearing a security hat, and removing it is the point of this
/// change.
///
/// The two shape refusals that stay are the ones with ZERO legitimate cost: a leading `-` and a
/// `.`/`..` path segment are not model ids on any arm, so keeping them out is free defence in depth
/// if such a value ever reaches a context that does interpret it. What is dropped is only what was
/// shown to reject real ids.
pub fn valid_catalog_model_id(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty()
        && model.len() <= CATALOG_MODEL_ID_MAX_CHARS
        && !model.starts_with('-')
        && !model.split('/').any(|segment| segment == ".." || segment == ".")
        && !model.chars().any(char::is_control)
        && !model.chars().any(char::is_whitespace)
}

/// Accept a model id only if it is safe to hand to a CLI as a `--model <id>` argument.
///
/// This became load-bearing the moment the catalog stopped being an allowlist. Previously the
/// Settings picker could only emit an id from a compile-time constant, so nothing arbitrary could
/// reach a provider. Now any string the user types is forwarded verbatim to
/// `claude --model <id>` / `codex exec --model <id>`, and a value like
/// `--sandbox danger-full-access` would be read by the CLI as a FLAG, not as a model name.
///
/// A model id is a slug, not free prose: ASCII alphanumerics plus `.`, `-`, `_`, `:` and `/`
/// (Ollama uses `family:tag`, gateways use `vendor/model`). Anything else — a leading `-`, any
/// whitespace or control character, or an unreasonable length — is rejected, and the caller falls
/// back to the provider's own default rather than passing a hostile value through.
pub fn valid_model_id(model: &str) -> bool {
    model_id_shape_is_safe(model.trim(), MODEL_ID_MAX_CHARS)
}

/// Bundled Claude model hints for the `claude_code` and `anthropic` connections.
///
/// Neither connection has a catalog endpoint Murmur can reach without shipping a key exchange, so
/// these are compile-time hints — no I/O, no egress. Order is display order, most capable first.
/// **This list going stale must not break anything**: `list_models` marks it `source: "bundled"`,
/// the picker always offers a free-text id, and nothing validates a stored id against it.
pub const CLAUDE_MODELS: &[CloudModel] = &[
    CloudModel {
        id: "claude-opus-5",
        label: "Claude Opus 5",
        note: "highest quality",
    },
    CloudModel {
        id: "claude-sonnet-5",
        label: "Claude Sonnet 5",
        note: "balanced",
    },
    CloudModel {
        id: "claude-fable-5",
        label: "Claude Fable 5",
        note: "creative work",
    },
    CloudModel {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        note: "fastest",
    },
    CloudModel {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
        note: "previous generation",
    },
];

/// Bundled Codex model hints for the `codex_cli` connection.
///
/// The Sol/Terra/Luna roles exposed by Codex CLI: highest-quality, balanced and fast. Selecting
/// one passes the id verbatim to `codex exec --model`. Same contract as [`CLAUDE_MODELS`] — a
/// hint, not an allowlist.
pub const CODEX_MODELS: &[CloudModel] = &[
    CloudModel {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        note: "highest quality",
    },
    CloudModel {
        id: "gpt-5.6-terra",
        label: "GPT-5.6 Terra",
        note: "balanced",
    },
    CloudModel {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        note: "fastest",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingMeta {
    /// e.g. "2026-06-24"
    pub date_iso: String,
    pub title_hint: Option<String>,
    pub duration_s: i64,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    pub transcript: String,
    pub meta: MeetingMeta,
    /// note-format prompt (summarize/template.rs)
    pub template: String,
    /// existing note titles → [[link]] targets
    pub vault_titles: Vec<String>,
    /// brain2 RAG Phase 4 — a small, GATED corpus of related PRIOR notes (each headed by a
    /// `### [[Title]] · date · id:` citation) so the model can ground the new note in past
    /// decisions/owed items. `None` (the default + flag-OFF case) ⇒ `render_user_content` is
    /// byte-identical to before this field existed. SECURITY: this string EGRESSES to the cloud
    /// provider in the summarization prompt, so it MUST be assembled only from VISIBLE
    /// (not sealed-not-unlocked) prior notes — see `summarize::related_context::build_related_context`.
    /// It is redacted by `RedactingProvider` alongside the transcript before egress.
    pub related_context: Option<String>,
    /// ENHANCE-MY-NOTES: the user's own typed in-meeting notes (the `manual_notes` buffer —
    /// raw `\n`-joined lines, NOT markdown bullets), present ONLY when `notes_mode == "enhance"`
    /// AND the buffer is non-blank. `None` ⇒ `render_user_content` is byte-identical to before
    /// this field existed (same contract as `related_context`). SECURITY: this string EGRESSES
    /// to the provider in the prompt — `RedactingProvider` MUST scrub it alongside the
    /// transcript (summarize/redact.rs) before egress. Today's append mode never egresses it.
    pub user_notes: Option<String>,
    /// Brain v2 L4 — the RUNNING LIVE BULLETS captured during the recording
    /// (`transcribe::bullets`), rendered by `render_user_content` as the "LIVE NOTES (auto)"
    /// section BEFORE the transcript. `None`/blank (no recording bullets — the default, the
    /// flag-off case, and every resummarize after the Stop-time consume) ⇒ the rendered prompt is
    /// byte-identical to before this field existed (same contract as `user_notes`). SECURITY:
    /// this string EGRESSES to the provider in the prompt — `RedactingProvider` MUST scrub it
    /// alongside the transcript (summarize/redact.rs) before egress.
    pub live_bullets: Option<String>,
    /// Bounded, structured workspace glossary rendered locally from Settings. `None` means the
    /// user prompt is byte-identical to the pre-glossary prompt. When present this EGRESSES in the
    /// note-generation prompt and MUST pass the same shared regex + name-redaction batch as the
    /// transcript. It is never included in the separate graph-extraction call.
    pub glossary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

/// Canonical system prompt used by the trait's free-text JSON fallback. Kept as a helper so the
/// egress ledger can measure the exact bytes the default path sends without reimplementing it.
pub(crate) fn default_json_system_prompt(system: &str, schema: &Value) -> String {
    format!(
        "{system}\n\nRespond with ONLY a single JSON object matching this schema (no prose, no code fences):\n{}",
        serde_json::to_string(schema).unwrap_or_default()
    )
}

#[async_trait]
pub trait SummarizerProvider: Send + Sync {
    /// Stable id: "claude_code" | "codex_cli" | "anthropic" | "ollama" | "gateway".
    fn id(&self) -> &str;

    /// Cheap, non-failing readiness probe (key set? ollama up? claude in PATH?).
    async fn availability(&self) -> Availability;

    /// Produce finished Obsidian-ready Markdown from the request.
    async fn summarize(&self, req: &SummarizeRequest) -> Result<String>;

    /// Raw completion: run a system + user prompt and return the model's text verbatim
    /// (no formatting/validation). Used for structured side-tasks like the timeline.
    async fn complete(&self, system: &str, user: &str) -> Result<String>;

    /// Like [`summarize`] but also returns token-usage and model metadata from the provider.
    ///
    /// Default implementation delegates to [`summarize`] and returns an empty [`CallMeta`].
    /// Providers that capture `usage` + `model` from their API response override this
    /// (and delegate [`summarize`] to this, stripping the meta) so the HTTP call is not
    /// duplicated.
    async fn summarize_with_meta(&self, req: &SummarizeRequest) -> Result<(String, CallMeta)> {
        Ok((self.summarize(req).await?, CallMeta::default()))
    }

    /// Like [`complete`] but also returns token-usage and model metadata from the provider.
    ///
    /// Default implementation delegates to [`complete`] and returns an empty [`CallMeta`].
    /// Providers that capture `usage` + `model` override this.
    async fn complete_with_meta(&self, system: &str, user: &str) -> Result<(String, CallMeta)> {
        Ok((self.complete(system, user).await?, CallMeta::default()))
    }

    /// Like [`complete_with_meta`] but carries [`crate::reason::GenOptions`] — a per-call token cap /
    /// sampler override for the on-device path (the note-edit runaway guard).
    ///
    /// DEFAULT = IGNORE the options and delegate to [`complete_with_meta`]. The cloud
    /// ([`super::redact::RedactingProvider`]) and other non-local providers have no local sampler to
    /// bound and keep their own limits + redaction — so ignoring the opts is correct and non-breaking.
    /// [`super::local::LocalSummarizerProvider`] OVERRIDES this to honor the cap on the GGUF path.
    async fn complete_with_meta_opts(
        &self,
        system: &str,
        user: &str,
        _opts: crate::reason::GenOptions,
    ) -> Result<(String, CallMeta)> {
        self.complete_with_meta(system, user).await
    }

    /// Structured-output completion returning the JSON value AND the provider's `CallMeta`
    /// (token usage + served model).
    ///
    /// DEFAULT = schema-in-prompt + `parse_first_json`, with meta from `complete_with_meta`.
    /// A provider that supports native constrained decoding (the OpenAI-compatible gateway)
    /// OVERRIDES this to send `response_format: {"type":"json_schema", …}` and returns both
    /// the parsed value and real token/model metadata directly.
    async fn complete_json_with_meta(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<(Value, CallMeta)> {
        // Default: embed the schema as a system-prompt instruction and call complete_with_meta
        // so token usage is captured even on the free-text path.
        let sys = default_json_system_prompt(system, schema);
        let (reply, meta) = self.complete_with_meta(&sys, user).await?;
        let v = crate::reason::parse_first_json::<Value>(&reply)?;
        Ok((v, meta))
    }

    /// Structured-output completion: return a JSON value adhering to `schema`.
    ///
    /// Delegates to [`complete_json_with_meta`] and drops the `CallMeta` so callers that
    /// only need the value are unchanged. Providers that support native constrained decoding
    /// override [`complete_json_with_meta`] instead; this default is then correct automatically.
    async fn complete_json(&self, system: &str, user: &str, schema: &Value) -> Result<Value> {
        Ok(self.complete_json_with_meta(system, user, schema).await?.0)
    }

    /// Does this provider enforce valid JSON NATIVELY (constrained decoding — e.g. the
    /// OpenAI-compatible `response_format: {"type":"json_schema", …}`) rather than
    /// schema-in-prompt + [`crate::reason::parse_first_json`] recovery?
    ///
    /// Brain v2 L3 structured-output hardening: a CAPABILITY SEAM ONLY for now — nothing
    /// dispatches on it yet (per spec §L3, `CloudReasoner` keeps its current path until the
    /// shadow data justifies a cutover). Default `false`; the gateway provider (which already
    /// sends `response_format` json_schema in `complete_json_with_meta`) overrides to `true`.
    fn supports_native_json(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 2.2 — a provider that does NOT override `*_with_meta` gets an empty `CallMeta`
    /// from the default implementations. This proves the additive default is wired correctly
    /// without touching any existing provider.
    struct FixedProvider(&'static str);

    #[async_trait]
    impl SummarizerProvider for FixedProvider {
        fn id(&self) -> &str {
            "fixed"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
            Ok(self.0.to_string())
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    /// Brain v2 L3 — `supports_native_json` DEFAULTS to false: a provider that does not override
    /// it is on the schema-in-prompt + `parse_first_json` recovery path (only the gateway, which
    /// sends `response_format: json_schema`, overrides to true — tested in `gateway.rs`).
    #[test]
    fn supports_native_json_defaults_to_false() {
        assert!(!FixedProvider("x").supports_native_json());
    }

    /// `complete_with_meta` default returns the plain `complete` output paired with empty `CallMeta`.
    #[tokio::test]
    async fn default_complete_with_meta_returns_empty_call_meta() {
        let p = FixedProvider("hello");
        let (text, meta) = p.complete_with_meta("sys", "usr").await.unwrap();
        assert_eq!(text, "hello");
        assert_eq!(
            meta,
            CallMeta::default(),
            "default impl must return empty CallMeta"
        );
    }

    /// Task 8.1 — `complete_json` default extracts the first JSON object from a prose-wrapped /
    /// fenced reply (the free-text path). A provider that does NOT override `complete_json` gets
    /// the schema embedded in the system prompt and the JSON extracted via `parse_first_json`.
    #[tokio::test]
    async fn default_complete_json_extracts_first_json_from_free_text() {
        // FixedProvider returns whatever string was given as its inner &str — simulate a model
        // that wraps the JSON in prose and a code fence (the worst-case free-text reply).
        struct JsonFencedProvider;
        #[async_trait]
        impl SummarizerProvider for JsonFencedProvider {
            fn id(&self) -> &str {
                "json-fenced"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                // Simulate a chatty model that wraps output in prose + a code fence.
                Ok("Sure! Here is the JSON:\n```json\n{\"people\":[\"Alice\"],\"projects\":[]}\n```\nLet me know if you need changes.".to_string())
            }
        }
        let schema = serde_json::json!({"type":"object","properties":{"people":{"type":"array"},"projects":{"type":"array"}}});
        let v = JsonFencedProvider
            .complete_json("SYS", "USER", &schema)
            .await
            .unwrap();
        assert_eq!(
            v["people"][0], "Alice",
            "default impl must extract JSON from fenced/prose reply"
        );
        assert!(v["projects"].as_array().unwrap().is_empty());
    }

    /// Task 8.1 — `complete_json` default returns an error when the reply contains no JSON object.
    #[tokio::test]
    async fn default_complete_json_errors_on_no_json() {
        struct NoJsonProvider;
        #[async_trait]
        impl SummarizerProvider for NoJsonProvider {
            fn id(&self) -> &str {
                "no-json"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok("I'm sorry, I cannot produce JSON right now.".to_string())
            }
        }
        let schema = serde_json::json!({});
        let err = NoJsonProvider.complete_json("SYS", "USER", &schema).await;
        assert!(
            err.is_err(),
            "no JSON in reply must yield an error from the default impl"
        );
    }

    /// `summarize_with_meta` default returns the plain `summarize` output paired with empty `CallMeta`.
    #[tokio::test]
    async fn default_summarize_with_meta_returns_empty_call_meta() {
        let p = FixedProvider("note body");
        let req = SummarizeRequest {
            transcript: "t".into(),
            meta: MeetingMeta {
                date_iso: "2026-06-30".into(),
                title_hint: None,
                duration_s: 60,
                language: None,
            },
            template: String::new(),
            vault_titles: vec![],
            related_context: None,
            user_notes: None,
            live_bullets: None,
            glossary: None,
        };
        let (text, meta) = p.summarize_with_meta(&req).await.unwrap();
        assert_eq!(text, "note body");
        assert_eq!(
            meta,
            CallMeta::default(),
            "default impl must return empty CallMeta"
        );
    }
}
