# Codex CLI 0.146.0 isolation fixture

These checked-in protocol samples were recorded locally on 2026-07-29 with synthetic markers only;
no Murmur or user content was sent. They are not a runner-owned attestation and the Rust fixture
test treats them only as parser-compatibility samples.

The command used the production `build_codex_command` policy: ephemeral execution, ignored user
config/rules, strict config, denied filesystem/network permission profile, `/var/empty` cwd,
web/apps/plugins/multi-agent disabled, and the wildcard `PreToolUse` deny hook.

- A normal text-only request completed successfully with `MURMUR_CODEX_SMOKE_OK`.
- A forced `/usr/bin/printf MURMUR_TOOL_SHOULD_NOT_RUN` attempt produced
  `Command blocked by PreToolUse hook` before execution. Its JSONL and stderr are retained beside
  this file.
- A forced `MURMUR_WEB_SHOULD_NOT_RUN` web search reported that no web-search tool was available
  and emitted no `web_search` event. Its JSONL is retained beside this file.

The Rust test `installed_codex_cli_accepts_the_exact_production_schema_when_available` resolves the
optional installed Codex binary and runs the exact production argv with synthetic stdin inside a
network boundary: a regular checkout adds an inner Seatbelt profile that denies all network access;
when Seatbelt reports that it cannot nest, the test additionally requires an `EPERM` result from a
non-loopback connection to the permanently reserved RFC 5737 TEST-NET-1 address before using the
inherited Harness boundary. That profile permits loopback only and blocks OpenAI. When the binary is
present, the test asserts that the real process emits `thread.started` and no flag/configuration
parse rejection under the unmodified production child environment, then writes an uncaptured
`MURMUR_CODEX_SCHEMA_EXECUTED version=… strict_profile_accepted=true production_env=true
thread_started=true network_boundary=…` marker into the runner's non-truncated stderr log. When
Codex is absent it
instead writes
`MURMUR_CODEX_SCHEMA_SKIPPED cli=absent`, so a green check is not mistaken for runner-owned schema
evidence.

The companion Rust test `immutable_tool_guard_command_emits_a_deny_decision` executes the exact
immutable command embedded in `hooks.PreToolUse`, parses its JSON decision, and writes
`MURMUR_CODEX_TOOL_GUARD_EXECUTED production_config_derived=true decision=deny` into the same runner
log. The production TOML hook config is derived from that exact tested command rather than carrying
a second escaped copy.

`installed_codex_cli_honors_the_production_tool_and_capability_boundary` points the real CLI at a
loopback-only synthetic Responses endpoint. The endpoint requests a shell command, observes the
exact production hook denial in the CLI's follow-up request, verifies that the command created no
side effect, and inspects the advertised tool identities for disabled web/MCP/plugin/multi-agent
capabilities. It emits `MURMUR_CODEX_RUNTIME_ISOLATION_EXECUTED production_env=true …`.

`installed_codex_cli_summarize_round_trips_a_normalized_note` uses the same real-CLI/loopback
boundary but returns fenced Obsidian markdown. It drives `CodexCliProvider::summarize` end to end,
including stdin rendering, CLI JSONL parsing, and note normalization, then emits
`MURMUR_CODEX_NOTE_ROUNDTRIP_EXECUTED production_provider=true jsonl_parsed=true
markdown_normalized=true …`.
