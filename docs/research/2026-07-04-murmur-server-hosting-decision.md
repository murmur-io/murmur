<!-- Generated 2026-07-04 via Workflow murmur-server-hosting-decision (4-angle research → synthesis → adversarial red-team, verdict SOUND_WITH_CHANGES). Pricing/AUP/law = point-in-time 2026-07-04. Not legal advice. -->
# Where to host the managed murmur-server — decision

## TL;DR

**Hetzner Cloud, EU (Falkenstein/Helsinki), operator EU-established (Poland), running the verbatim docker-compose artifact; backups age-encrypted → Cloudflare R2 (+ WAL archiving for RPO-minutes).** The spec's Hetzner pick was right — it survived a 4-angle pressure-test and an adversarial red-team. But the red-team killed two things the synthesis got wrong, both **irreversible one-way doors**, so they matter most:

1. **Do NOT put a "second box" in the same Hetzner account for isolation** — Hetzner's catastrophic blast radius is **account-level termination**, not per-IP. A sibling box protects against nothing that matters. For real reputation isolation of the immortal share surface, put it **on a different provider (Scaleway-FR)** — or accept **two-domains-one-box + a rehearsed DNS re-point runbook** for the beta.
2. **Register the immortal share domain at an EU registrar (Gandi/INWX), never Cloudflare/US** — share links are immortal, and a US-domiciled registrar (CLOUD Act / UDRP / AUP seizure) can permanently break every issued link. Cloudflare-as-DNS is fine (re-pointable); Cloudflare-as-registrar-of-the-one-way-door is not.

And the biggest strategic finding: **launch self-host-first; gate the *managed* instance on abuse-desk coverage or a paid tier** — a solo operator structurally cannot meet Hetzner's ~6–48h abuse-response windows / DSA "without undue delay", and a missed window = nullroute = an auth outage for *other people*.

## The recommendation (concrete)

| Piece | Choice | Why |
|---|---|---|
| **Compute** | Hetzner Cloud **CX22** (~€5.49/mo, EU FSN/HEL), the exact self-host `docker-compose` (server + `postgres:17` + Caddy + backup cron) | Only the bare-VPS category preserves "hosted == self-host artifact" (spec §2/§5); every PaaS/managed-DB split breaks it and costs 5–10×. EU metal = on-brand + GDPR home turf. |
| **DB** | `postgres:17` in-compose, single-node, no failover **by design** (RPO ≤24h, RTO manual); `pgdata` on a Hetzner **Volume** (€0.057/GB-mo) so it grows without rescaling the box | Volume-not-bigger-box is the cheap disk hedge; a *rescale* re-prices at new tiers, a Volume doesn't. |
| **Backups** | **age-encrypted `pg_dump` → Cloudflare R2** (zero egress) **+ WAL archiving → R2** (WAL-G/pgBackRest sidecar, RPO≈minutes, ~€0) | Nightly-only loses a day of signups/shares on box death; WAL closes it for free. R2's US jurisdiction is fine — dumps are `age`-encrypted client-side, so a CLOUD Act demand yields only ciphertext (optics, not confidentiality). |
| **Share-surface topology** | **Beta: two-domains-ONE-box** (Caddy vhosts) + DNS re-point runbook. **Before public links: share surface on a DIFFERENT provider (Scaleway-FR)** | Hetzner terminates at the account level → a second box in the same account is false comfort. True isolation needs a different account/provider. |
| **DNS** | Cloudflare DNS (re-pointable) | Fine for DNS; NOT for registrar of the immortal name. |
| **Registrar (immortal share domain)** | **EU registrar (Gandi / INWX)**, decoupled from Cloudflare | The one un-undoable asset must not sit under US control. App/API domain can be anywhere. |
| **Email** | `lettre` SMTP → Resend/SES on :587 (open on Hetzner) | Host-independent line item; never self-run SMTP. |
| **Jurisdiction** | Operator EU-established (Poland) | No DSA Art. 13 legal-rep (EU-only); GDPR home; report-with-key satisfies DSA Art. 16/17 + Hetzner's item-level removal. |

## Why (the decisive reasoning)

1. **The "hosted == self-host artifact" invariant eliminates PaaS, hyperscaler, and managed-DB-split before cost or law** — all four briefs converged independently. Fly steers you to Managed Postgres (the OPAQUE `ServerSetup` singleton then lives outside your `pg_dump`/R2 scope → total-login-loss on restore); Railway/Render decompose a compose file. Splitting the DB out pays a premium to lose the exact property that makes the self-host story honest.
2. **Zero-knowledge already won the jurisdiction fight** — the CLOUD Act is encryption-neutral (a demand against a ZK host yields only ciphertext regardless of country; AWS's own CLOUD Act page confirms). So Switzerland/Iceland buy a redundant secrecy edge + foreign-incorporation drag. EU metal is the marketable, cheap, correct answer, and the operator already lives in the EU.
3. **Cost is trivial and identical across the EU-VPS field** (~€5.50 beta → ~€40–60/mo planning headroom at 10k), so the tie-breakers are **abuse-desk survivability** and **artifact fit** — where the red-team's account-kill + registrar findings reshape the topology.

## Cost (all-in, EUR/mo, point-in-time 2026-07-04; budget generously — Hetzner repriced UP twice in 2026)

| Users | Compute | DB | Backups | Email | DNS | Total (plan) |
|---|---|---|---|---|---|---|
| 0 (beta) | 1× CX22 €5.49 | in-compose €0 | R2 free | €0 | ~€0 | **~€5.50** |
| 100 | 1–2 surfaces €5.5–11 + Volume €0.60 | €0 | free | €0–5 | ~€1 | **~€12–20** |
| 1,000 | 2 surfaces €11 + Volume €1–3 | €0 | free | ~€5 | ~€1 | **~€18–25** |
| 10,000 | grow via **Volume / new CX box, NEVER rescale into CPX** (CPX22 tripled to €19.49) | €0 | ~€2 | ~€10 | ~€1 | **~€40–60 headroom** |

The 20 TB Hetzner traffic bundle means a phishing-driven spike on the share domain is not a billing event (contrast metered-egress PaaS).

## Legal posture this locks in

- **GDPR data controller** for account emails + share metadata (content is E2E-unreadable). Poland/EU home. No DPO, largely Art. 30-exempt (<250 staff). Forced: privacy policy (publish the §6 metadata list verbatim), ToS (min age 16), signed DPAs (Hetzner, ESP, Cloudflare/R2), the `DELETE /account` + `GET /account/export` endpoints, breach readiness.
- **DSA:** Art. 11/12 + 16/17 mandatory at any size (already designed); Art. 13 legal-rep NOT required (EU-established); Art. 19 micro-enterprise exclusion exempts the heavy platform machinery. The **report-with-key flow** (reporter consents to include the fragment key → operator decrypts the single item → adjudicate/delete) is what makes DSA satisfiable for opaque content and maps onto Hetzner's own removal process.
- **CSAM:** §2258A (actual-knowledge NCMEC report, no scan duty). Text-only sanitized render + no-attachments makes shares a poor vector.
- **Abuse response is an operational duty, not a task** — Hetzner windows ~6–48h; DSA "without undue delay." A monitored abuse mailbox + same-day keyed-report→delete runbook is mandatory. **A solo dev cannot guarantee this → see the go/no-go gate.**

## The two irreversible mistakes the red-team caught (do NOT get these wrong)

- **Account-level blast radius:** community evidence (HN #42365295, LowEndTalk #198162 / #195720) shows Hetzner cancels *accounts* and nullroutes *all* servers off an abuse chain, not one IP. A second box in the same account = extra cost, false isolation. → different provider for the share surface, or one-box + DNS re-point.
- **Registrar jurisdiction of the immortal domain:** links are immortal; a US registrar (Cloudflare, Inc., CLOUD-Act/UDRP-subject) can seize/suspend the one-way-door name and break every link. → EU registrar for the share domain.

## Migration triggers (when to move)

- Scale past ~10k / RPO-24h unacceptable → managed EU DBaaS (accept artifact divergence; document a separate self-host backup path). Not before.
- Audio-blob growth (tens of MB) → implement the `BlobStore` trait + additive `storage_ref` column, offload blobs to R2 (a coded refactor, not a host move).
- A real Hetzner abuse experience goes bad → fail over to **Scaleway-FR** (content-based AUP, no anonymization ban). **Never OVH** (ToS bans "public Proxy/VPN/anonymization"). Contabo = cheap emergency-only (oversubscription reputation). A *second Hetzner account is not a fix* (Hetzner links accounts).
- EU CSAR ("chat control") passes with a hosting-provider detection limb → re-assess (5th trilogue was 29-Jun-2026; outcome not public as of 2026-07-04 — re-check before immortalizing the share domain). A true ZK host cannot scan; compliant posture = filed risk-assessment + report-with-key.

## Smallest first step (de-risking spike, on throwaway domains before buying the immortal name)

1. Provision one CX22, `docker compose up -d` the **exact self-host artifact**, create an account, log in (proves the artifact runs clean on stock Hetzner).
2. **Wire + run the backup** — `scripts/backup.sh` exists but has **no runner and no `pg_dump`/`age`/`rclone` in the `bookworm-slim` runtime image** (a real launch blocker). Add a sidecar cron container (or host crontab `docker exec`). Add WAL archiving → R2.
3. **Run the T4.1 restore drill end-to-end: restore into a fresh `postgres:17`, boot, confirm the same user still logs in.** THE single most important validation — because ServerSetup + OPAQUE material live in Postgres, a bad backup is *total login loss*.
4. Confirm Caddy TLS on both vhosts.
5. Load-check OPAQUE + Argon2id (m=64 MiB) p95 latency under concurrency on shared-vCPU CX22 (the only reason to consider a stronger box — and the answer is a Volume-backed *new* CX box, not a CPX rescale).

## Written decisions (not yet actions)

- **The NAME** (upstream blocker — trymurmur.app twin) gates *which two domains you buy*. Resolve before immortalizing the share domain.
- **Self-host-first vs managed-launch = a go/no-go gate**, not a migration trigger: ship the compose for self-hosters now; stand up the managed instance only once there's abuse-desk coverage (on-call / second operator) or a paid tier that funds it (spec §14 flags paid as the funding path). Murmur is local-first, so the managed relay is a convenience layer, not load-bearing — self-host-first is the lower-risk launch.
- **A one-hour lawyer review** of ToS + privacy policy + the §2258A-extraterritoriality / CSAR nuances (a researcher's read is not counsel).
- **Wire the hourly GC task + global storage cap in `main.rs`** (currently per-request helpers only) before public launch — the disk-fill guard depends on it.

## Open questions

- Exact live CX22 price/SKU (sources scatter €3.79–5.49; an apparent CX22→CX23 rename post-15-Jun-2026) — confirm on the live console at provision (existing instances hold rate; a rescale re-prices).
- The exact **share-serving data path** if the share surface goes on a different provider (proxy box A's API vs read-replica) — interacts with the single-node-DB posture; a design decision before buying.
- Hetzner's real reaction to an E2E-content abuse ticket + the "deleted the item, can't scan the rest" DSA response — pre-email Hetzner abuse describing the ZK model as a de-risking probe.
- CSAR final text; §2258A binding force on a purely-EU operator — lawyer/moving-target items, re-check at deploy.

## Sources (point-in-time 2026-07-04; full briefs in the workflow transcript)

Hetzner pricing + Jun-2026 adjustment (CPX22 €7.99→€19.49): https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/ · Cloudflare R2 pricing (zero egress): https://developers.cloudflare.com/r2/pricing/ · AWS CLOUD Act (encryption-neutral): https://aws.amazon.com/compliance/cloud-act/ · Cloudflare US-jurisdiction: https://danubedata.ro/blog/cloudflare-r2-alternatives-europe-2026 · Scaleway abuse AUP (content-based): https://www.scaleway.com/en/abuse-notice/ · OVH ToS (bans proxy/VPN/anonymization): https://us.ovhcloud.com/legal/service-specific-terms/ · Hetzner account-termination reports: https://news.ycombinator.com/item?id=42365295 , https://lowendtalk.com/discussion/198162 , https://lowendtalk.com/discussion/195720 · spec `docs/superpowers/specs/2026-07-04-murmur-server-spec.md` §5/§9, DEPLOY.md, `docker/docker-compose.yml`.
