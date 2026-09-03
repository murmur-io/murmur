# Use Murmur with your own AI agent

Murmur records your meetings, transcribes them on-device, and writes clean Markdown
notes straight into your Obsidian vault. If you also run **Claude Code** (or any other
MCP- or Skills-capable agent) over that same vault, this pack teaches your agent to
**cooperate with Murmur instead of fighting it** — to know which parts of a note are
Murmur-managed, to link the way Murmur links, and to reach Murmur's gated, indexed
knowledge through a local read-only server instead of blindly grepping files.

This is a set of **plain files** — you own them, they never phone home, and nothing here
changes Murmur itself.

## What's in the pack

| File | For | Teaches |
| --- | --- | --- |
| [`murmur-vault/SKILL.md`](murmur-vault/SKILL.md) | your agent | The anatomy of a Murmur note — front-matter keys, managed sections it must never edit, how wikilinks work, entity stubs, where things live, and the coexistence contract (add freely, prefer append, never touch managed regions). |
| [`murmur-mcp/SKILL.md`](murmur-mcp/SKILL.md) | your agent | When and how to query Murmur's local read-only MCP server (gated, indexed retrieval) instead of grepping raw files, plus the full twenty-tool catalog. |

There is also a human-facing setup guide at
[`docs/USE-WITH-YOUR-AGENT.md`](../docs/USE-WITH-YOUR-AGENT.md).

## Installing the skills into Claude Code

Agent Skills are directories containing a `SKILL.md`. Claude Code discovers them from two
locations:

- **Personal (all your projects):** copy each skill directory into `~/.claude/skills/`.
- **Project-scoped (checked in for a repo/vault):** copy them into `.claude/skills/` at the
  root of the project (here, your vault).

```bash
# Personal install (both skills, for every project):
mkdir -p ~/.claude/skills
cp -R murmur-vault murmur-mcp ~/.claude/skills/

# …or project-scoped, from inside your vault:
mkdir -p .claude/skills
cp -R /path/to/murmur/vault-skills/murmur-vault /path/to/murmur/vault-skills/murmur-mcp .claude/skills/
```

Each skill's YAML front-matter (`name` + `description`) is what Claude reads to decide when
the skill is relevant; the body loads only when it's actually needed. After copying, start a
new Claude Code session in the vault so the skills are picked up.

## A word on privacy

An agent that reads your vault as raw files sees **every unsealed note in full** — Murmur's
notes and your own, plaintext on disk. That's the nature of any file-reading agent, and it's
worth being deliberate about which vault you point it at. Murmur's own privacy line holds even
here: a **locked (sealed) folder's `.md` files are physically removed from disk** while locked,
so a file-reading agent cannot see sealed content at all (see the disappeared-file rule in the
vault skill). For access that is **gated, indexed, and read-only** — semantic + full-text search,
entity dossiers, structured transcripts, typed-property queries, all of which exclude sealed
content — point your agent at Murmur's **local MCP server** instead of grepping files. It listens
on loopback only (`127.0.0.1:8765`), is token-gated by default, and never sends anything to the
cloud. The MCP skill covers the setup.
