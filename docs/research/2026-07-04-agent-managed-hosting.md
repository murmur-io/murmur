<!-- Generated 2026-07-04 via Workflow agent-managed-hosting (4-angle research → synthesis → adversarial red-team, verdict SOUND_WITH_CHANGES). Point-in-time; not legal advice. -->
# Can an AI agent fully manage the murmur-server hosting? — decision

## TL;DR

**The TECH ops loop is fully agent-manageable; the abuse/legal loop is NOT — you shrink it to near-zero by launching invite-only/closed, but a reachable human legal person stays irreducibly on-call for rare events.**

The honest headline (red-team's key truth): *the agent does not close the cadence/legal gap — the closed-beta scope does; the agent just makes the shrunken residual cheap to run.* The human is converted from a **continuous ops on-call** into a **low-frequency, un-schedulable, hard-clocked pager** that a legal person must still answer whenever the rare event (abuse notice / law-enforcement / CSAM) fires. "Fires rarely" ≠ "can be scheduled."

## The architecture (what the agent runs)

- **Infra: Hetzner Cloud (EU), bare VMs, declared in Terraform** (`hetznercloud/hcloud` v1.66) with remote state (HCP Terraform), deployed by **GitHub Actions**, `hcloud` CLI for imperative ops (snapshots). This is the only option that is *both* fully agent-operable (every lifecycle verb = a CLI/API/git-commit call, no console) *and* runs the `docker-compose` artifact **verbatim** (preserving hosted == self-host). Fly is more API-native (official `fly mcp server`) but breaks the artifact via `fly.toml`+`[mounts]` config drift; Render/Railway dismantle it (managed-DB split). Bare-VPS + GitOps resolves the agent-manageability-vs-artifact conflict.
- **Deploy: GitOps** — the agent commits IaC/compose to `murmur-io/murmur-server`, GitHub Actions deploys to the box. The agent never SSHes by hand.
- **Ops loop: a scheduled Claude cloud-agent** (cron cadence) that hits `/healthz`, checks disk/blob-bytes, runs the nightly age-encrypted `pg_dump → R2` + WAL archiving (RPO-minutes), and periodically runs the **restore drill** (scratch DB → restore → assert a canary user logs in). An external monitor → `repository_dispatch` → an incident agent handles alerts.
- **Abuse: report-with-key** — a reporter consents to include the fragment key; the agent can *draft* a disposition and auto-*ack* (DSA Art 16(4), automated processing is permitted if disclosed) and auto-*disable-to-410* (reversible), but a **human approves every outbound legal notice** (Art 16(5)/17) and the **keyed-decrypt-to-verify** step (it can create §2258A actual-knowledge).

## The irreducible human residue (an agent legally cannot be/do)

- Be the **GDPR data controller** / a legal person; sign DPAs; accept a ToS; provide a **payment method**; be the **DSA Art 11/12 point of contact**; **§2258A NCMEC reporter of record** (+ 1-year preservation); respond to a **subpoena / law-enforcement**.
- Approve any **irreversible or liability-bearing** act: the outbound Art 16(5)/17 statement of reasons (even "clear-cut"), the keyed-decrypt of reported content, a destructive infra op.

## Guardrails (non-negotiable — from the red-team's required changes)

1. **Gate the DEPLOY, not just the agent's shell.** The real catastrophe vector isn't `rm -rf` in the agent's terminal (deny-hook blocks it) — it's the agent committing a poisoned `docker-compose.yml`/`scripts/backup.sh` that CI then deploys autonomously. → every prod-affecting change (compose/image/backup script) goes behind a **human-reviewed GitHub `production-destructive` environment**, OR CI applies only a **signed, allow-listed diff** (image-tag-only bumps); any change to the compose/backup scripts is hard-gated.
2. **No auto-send of legal notices.** Agent auto-acks + auto-disables (reversible); a human approves every Art 16(5)/17 outbound reply.
3. **Human-gate the decrypt-to-verify** of reported content (not just the CSAM disposition).
4. **Immutable backups:** verify the R2 bucket-lock is compliance-mode/WORM (owner cannot shorten) before calling it a floor; until then add a second, separately-credentialed offsite copy.
5. **Env-gate any operator beacon** (e.g. healthchecks.io ping) OUT of the shared self-host artifact (`HEALTHCHECK_PING_URL` empty by default) — a call-home in a zero-knowledge product is a regression.
6. **Right-size DSA** via the Art 19 micro/small-enterprise exemption (skip the Art 24(5) Transparency-DB machinery) — but keep the Section-2 (Art 16/17) human-approval residue.
7. **GitHub is a responder SPOF** — the external monitor must page the human directly too, not only via `repository_dispatch`.
- Plus: **least-privilege scoped API tokens** (separate destructive-op token behind a gate), **spend caps** at the provider, an **allowlisted runbook**, plan→approve→apply for `terraform apply`, an **audit log**, a **kill-switch**.

## The honest scope

- **Closed / invite-only launch (no anonymous public link creation):** an agent can run essentially the entire loop; abuse tickets ≈ 0, so the human is a rare on-call. **This is the recommended first launch anyway.** ✅
- **Public launch (strangers create shares):** the abuse surface grows → the un-schedulable human-legal SLA becomes real load → do NOT go solo-agent-managed; needs abuse-desk coverage (on-call rotation / a paid tier that funds it). ⚠️

## Cost + first step

- Cost is the plumbing cost (~€5–20/mo, unchanged from the prior hosting decision) + the agent's own scheduled-run tokens (small).
- **First step:** the one human act — create the Hetzner account + payment, hand the agent a scoped `HCLOUD_TOKEN` (Read&Write). Then the agent: generates+registers an SSH key, `terraform apply` a CX22 (Debian) + Volume, GitOps-deploys the compose, wires the scheduled ops agent, and runs the restore drill. Everything after the token is the agent's.

## Open questions

- R2 bucket-lock override authority (WORM vs owner-removable) — verify before trusting the backup floor.
- The exact incident-responder fallback path if GitHub Actions is itself down.
- CSAR final text (5th trilogue 29-Jun-2026, outcome not public 2026-07-04) — a hosting-provider detection-order limb would change the abuse posture.

## Sources (point-in-time 2026-07-04)

DSA Art 16 (auto-processing permitted if disclosed): https://www.eu-digital-services-act.com/Digital_Services_Act_Article_16.html · DSA Art 19 micro/small exemption: https://www.eu-digital-services-act.com/Digital_Services_Act_Article_19.html · 18 U.S.C. §2258A (actual-knowledge, 1-yr preservation): https://www.law.cornell.edu/uscode/text/18/2258A · Cloudflare R2 bucket locks (override authority undocumented): https://developers.cloudflare.com/r2/buckets/bucket-locks/ · Hetzner Terraform provider: https://github.com/hetznercloud/terraform-provider-hcloud · Fly Machines API / compose caveat: https://fly.io/docs/machines/api/machines-resource/ , https://fly.io/docs/machines/guides-examples/multi-container-machines/ · prior decision: docs/research/2026-07-04-murmur-server-hosting-decision.md.
