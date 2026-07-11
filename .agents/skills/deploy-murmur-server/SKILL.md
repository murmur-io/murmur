---
name: deploy-murmur-server
description: Deploy, redeploy, monitor, debug, and manage the murmur-server backend (the zero-knowledge E2EE sharing relay) on Railway. The complete, hard-won flow — project/service IDs, the token model, the GraphQL-API-not-CLI approach, deploy/logs/vars/domain/restore, and every gotcha that bit during the first deploy (env-freeze on redeploy, the Postgres name-collision DNS trap, raw-postgres password-init, the Dockerfile fixes). Use whenever the user wants to deploy/redeploy/check/fix/scale the Murmur server, read its logs, change its env, or manage its Railway hosting.
---

# Deploy & manage murmur-server on Railway

The Murmur backend (`murmur-io/murmur-server`, private) runs on **Railway**. This skill is the full operational playbook — it captures the exact flow + the gotchas that cost hours on the first deploy so they never bite again.

## What's deployed (the live instance)

| Thing | Value |
|---|---|
| Repo | `murmur-io/murmur-server` (private, AGPL-intended) — local at `/Users/jakubgawronski/Projects/murmur-server` |
| Public URL | `https://api.murmurnotes.io` (`/healthz` → `{"status":"ok",...}`) |
| Fallback Railway URL | `https://murmur-server-production-b9e8.up.railway.app` |
| Railway project | `murmur-server` · id `27e7b5a4-b5db-40ad-a63a-2a4d8276b29b` |
| Environment | `production` · id `8ef5a21d-96c6-4141-80ed-be4b7fc2f855` |
| Server service | `murmur-server` · id `4d55d97c-1a99-4a51-a947-657a012a9cfc` |
| Database | a **managed Railway Postgres** named `Postgres` · id `570954b2-32bc-42f4-b6c1-80d76959951d` — the server reads it via the variable reference `DATABASE_URL = ${{Postgres.DATABASE_URL}}` |
| Custom domain | `api.murmurnotes.io` · id `c225ac0a-0494-4682-b459-cf5c8e234bd4` · CNAME target `k9sfnbwk.up.railway.app` |
| GraphQL API | `https://backboard.railway.com/graphql/v2` |

The server has **no AI** on it — it's a dumb zero-knowledge relay (ciphertext blobs + auth + key relay). So it needs no GPU; a tiny instance is plenty.

## Current CI/CD

The backend no longer depends on Railway's GitHub integration. That integration could not access the
private repo ("no one in the project has access to it"), so deploys are GitHub Actions driven:

- Repo workflow: `murmur-io/murmur-server/.github/workflows/ci.yml`
  - PR and `main` checks: `gate`, `supply-chain`, `docker-build`.
  - Rust toolchain: `1.96.1` via `rust-toolchain.toml`.
  - Docker build: `docker/Dockerfile` (`rust:1.96.1-bookworm`).
- Repo workflow: `murmur-io/murmur-server/.github/workflows/deploy.yml`
  - Trigger: `workflow_run` after green `ci` on `main`, plus manual `workflow_dispatch`.
  - Deploy command: `npx --yes @railway/cli@5.23.3 up --ci --project <PID> --environment <EID> --service murmur-server`.
  - Secret: GitHub repo secret `RAILWAY_TOKEN` holds `~/.murmur/railway-projtoken`.

Important: Railway env var changes still require a fresh `railway up` deployment. Use the deploy
workflow manually after `variableUpsert` if only env changed.

## Tokens — ALREADY PERSISTED (no need to ask the user)

The user does NOT touch Railway. The credentials are already stored so any future session can manage the deploy hands-off:
- **`$RAILWAY_API_TOKEN`** — the Railway account/team token when Codex has it in the tool environment. In Codex, expose it from the parent shell or user-local `~/.codex/.env`; if it is absent, use the fallback file below. Use it for the GraphQL API (`Authorization: Bearer $RAILWAY_API_TOKEN`).
- **`~/.murmur/railway-token`** (600) — the same account/team token, on disk (fallback if the env var is absent).
- **`~/.murmur/railway-projtoken`** (600) — the project token scoped to this project+env, for `RAILWAY_TOKEN=... railway up`.
- **`~/.murmur/railway-ids`** — the project / env / server-service / postgres-service ids + the domain, one per line (same as the table above).

⚠️ SECURITY: the account/team token has broad access to the user's whole Railway account (not just this project). Store it only in user-local files (`~/.codex/.env` if used, plus `~/.murmur/`, both non-repo, 600). The user chose convenience over scoping. If it's ever exposed, rotate it at `railway.com/account/tokens` and update both locations. Note: a project-scoped token does NOT work for the GraphQL API (only for `railway up`/`status`), so the team token is the minimum for full management.

## Tokens (the auth model — this tripped us up)

Railway has **two** token types with different powers:

- **Account/Team token** — created at `railway.com/account/tokens` (NOT the workspace "Developer" page, which is OAuth apps). Use it as the `Authorization: Bearer <tok>` header for the **GraphQL API** (create projects/services, set vars, deploy, read logs). ⚠️ The CLI `railway whoami`/`railway list` FAIL with a team token because they call the `me` query (a team token has no user) — but `railway status`/`railway up` work, and the API `projects` query works. Don't be fooled by "Unauthorized" on `whoami`.
- **Project token** — created via the API (`projectTokenCreate`) for a specific project+environment. Use it as `RAILWAY_TOKEN=<tok>` for `railway up` / `railway status` (the CLI's native path).

The user provides an account/team token (paste it in chat, then **revoke it after** at `railway.com/account/tokens`). Store it locally to avoid re-typing:
```bash
umask 077 && printf '<TOKEN>' > /tmp/.railway-token   # account/team token
```
Create a project token from it (once per project) for `railway up`:
```bash
# needs the project + environment ids
mutation { projectTokenCreate(input: { name: "deploy", projectId: "<PID>", environmentId: "<EID>" }) }
# store it: printf '<projtoken>' > /tmp/.railway-projtoken
```

## The GraphQL helper (use this for everything API-side)

⚠️ Railway's API 403s the default Python-urllib User-Agent — **always send a `User-Agent` header**. Reusable pattern:
```python
import json, os, urllib.request
TOK=os.environ.get("RAILWAY_API_TOKEN") or open(os.path.expanduser("~/.murmur/railway-token")).read().strip()
PID="27e7b5a4-b5db-40ad-a63a-2a4d8276b29b"; EID="8ef5a21d-96c6-4141-80ed-be4b7fc2f855"
def gql(q):
    req=urllib.request.Request("https://backboard.railway.com/graphql/v2",
        data=json.dumps({"query":q}).encode(),
        headers={"Authorization":f"Bearer {TOK}","Content-Type":"application/json","User-Agent":"murmur-deploy/1.0"})
    r=json.load(urllib.request.urlopen(req))
    if r.get("errors"): raise SystemExit("ERR:"+json.dumps(r["errors"]))
    return r["data"]
```
(`curl -H "Authorization: Bearer $TOK" -H "User-Agent: x" -H "Content-Type: application/json" -d '{"query":"..."}' https://backboard.railway.com/graphql/v2` works too.)

## Common operations

### Deploy / redeploy the server code
`railway up` is the ONLY reliable way to deploy with the CURRENT env (see the env-freeze gotcha). It uploads the local dir (respecting `.gitignore`, so `/target` is skipped) and builds the Dockerfile.
```bash
cd /Users/jakubgawronski/Projects/murmur-server
export RAILWAY_TOKEN="$(cat ~/.murmur/railway-projtoken)"
railway up --service murmur-server --detach   # prints a Build Logs URL; build runs on Railway
```

### Check deployment status
```python
def st(sid):
    e=gql(f'query {{ deployments(first:1, input:{{projectId:"{PID}", environmentId:"{EID}", serviceId:"{sid}"}}){{ edges {{ node {{ id status }} }} }} }}')["deployments"]["edges"]
    return (e[0]["node"]["id"], e[0]["node"]["status"]) if e else (None,"NONE")
# statuses: BUILDING -> DEPLOYING -> SUCCESS | FAILED | CRASHED
```

### Read logs (the debugging workhorse)
```python
did,_=st("4d55d97c-1a99-4a51-a947-657a012a9cfc")   # server service id
# build failures:
for m in gql(f'query {{ buildLogs(deploymentId:"{did}", limit:40){{ message }} }}')["buildLogs"]: print(m["message"][:200])
# runtime/boot failures (crash loops, DB errors):
for m in gql(f'query {{ deploymentLogs(deploymentId:"{did}", limit:40){{ message }} }}')["deploymentLogs"]: print(m["message"][:200])
```

### Set / change an env variable
```python
gql(f'mutation {{ variableUpsert(input:{{projectId:"{PID}", environmentId:"{EID}", serviceId:"<svc>", name:"NAME", value:"VALUE"}}) }}')
# read current values (returns a name->value map):
gql(f'query {{ variables(projectId:"{PID}", environmentId:"{EID}", serviceId:"<svc>") }}')
```
⚠️ A variable change only takes effect on the NEXT deploy that reads CURRENT env — for the server that means `railway up` (NOT `serviceInstanceRedeploy`, see gotcha). Server vars: `DATABASE_URL=${{Postgres.DATABASE_URL}}` (reference), `SHARE_BASE_URL=https://<domain>`, `RUST_LOG=info`. `PORT` is injected by Railway; the server binds `0.0.0.0:$PORT`.

### Generate / see the public domain
```python
gql(f'mutation {{ serviceDomainCreate(input:{{serviceId:"<svc>", environmentId:"{EID}"}}){{ domain }} }}')
```

### Verify it's live
```bash
curl -s https://api.murmurnotes.io/healthz                               # -> {"status":"ok","version":"0.1.0"}
curl -s https://murmur-server-production-b9e8.up.railway.app/healthz      # fallback
# full end-to-end (blob round-trip, unauth path):
URL=https://api.murmurnotes.io
BID=$(curl -s -X POST "$URL/v1/blobs" --data-binary 'test' -H 'content-type: application/octet-stream' | python3 -c "import sys,json;print(json.load(sys.stdin)['blobId'])")
curl -s "$URL/v1/blobs/$BID"; echo; curl -s -o /dev/null -w "%{http_code}\n" "$URL/v1/blobs/00000000-0000-4000-8000-000000000000"  # -> 404
```

### Check the custom domain + cert
```bash
dig +short api.murmurnotes.io CNAME @1.1.1.1
dig +short _railway-verify.api.murmurnotes.io TXT @1.1.1.1
RAILWAY_TOKEN="$(cat ~/.murmur/railway-projtoken)" \
npx --yes @railway/cli@5.23.3 domain status api.murmurnotes.io \
  --project 27e7b5a4-b5db-40ad-a63a-2a4d8276b29b \
  --environment 8ef5a21d-96c6-4141-80ed-be4b7fc2f855 \
  --service murmur-server \
  --json | jq '{verified:.domain.verification.verified, cert:.domain.certificate.status, current:.domain.dnsRecords[0].currentValue}'
```

Expected DNS:
- `api CNAME k9sfnbwk.up.railway.app`
- `_railway-verify.api TXT railway-verify=f5bf42c0232021570fa6a25f0d7906dcda7992b5d130b8d7ddecdda1a228445b`

Expected Railway status: `verified=true`, `CERTIFICATE_STATUS_TYPE_VALID`.

### List services / find IDs
```python
for e in gql(f'query {{ project(id:"{PID}"){{ services {{ edges {{ node {{ id name }} }} }} }} }}')["project"]["services"]["edges"]:
    print(e["node"]["name"], e["node"]["id"])
```

## GOTCHAS — the hard-won lessons (do not relearn these)

1. **`serviceInstanceRedeploy` / `serviceInstanceDeploy` FREEZE the env.** They replay a past deployment's config and IGNORE variable changes. To apply new vars to the server, use **`railway up`** (creates a genuinely new deployment snapshotting current env). This wasted the most time — the server kept connecting with a stale `DATABASE_URL`.

2. **Postgres name collision breaks `*.railway.internal` DNS.** Two services named `Postgres` and `postgres` both map to `postgres.railway.internal` → the server hits the WRONG one (different password) → `password authentication failed`. Keep exactly ONE Postgres. **Use the MANAGED Railway Postgres** (has `DATABASE_URL`, `PGHOST`, `PGPASSWORD` vars) and point the server at it with the reference `DATABASE_URL=${{Postgres.DATABASE_URL}}` — never a raw `postgres:16` image.

3. **A raw `postgres` image sets its password only on FIRST init** — and a persistent volume makes it "skip initialization" forever, so later `POSTGRES_PASSWORD`/`PGDATA` changes are ignored and the DB keeps the stale password. Don't fight it — use the managed Postgres. (If you ever must use a raw image: no volume = fresh init every deploy = ephemeral; or `PGDATA=/var/lib/.../pgdata-subdir` avoids the `lost+found` initdb error, but you STILL need a fresh empty subdir + a fresh-env deploy.)

4. **Dockerfile (`docker/Dockerfile`) requirements** (already fixed + committed): build on the **full `rust:1.96-bookworm`** (the `-slim` variant lacks the C toolchain `ring`/rustls needs, and deps now require `rustc >= 1.88`); **no cargo-chef** (its `cargo install` step failed and Railway doesn't persist its cache); do **NOT** hardcode `ENV BIND_ADDR` (let `$PORT` win so Railway can route). Railway DOES cache Docker layers, so repeat `railway up` builds are fast.

5. **`railway.json`** (committed at repo root) sets `build.dockerfilePath = docker/Dockerfile` + a `/healthz` healthcheck. Without it Railway won't find the non-root Dockerfile.

6. **The CLI is finicky with tokens:** `railway add`/`railway link` fail ("Project not found") with a project token — provision databases via the **API** (or the dashboard), not `railway add`. `railway up`/`railway status` are the CLI commands that work.

7. **Migrations run automatically** on server boot (`sqlx::migrate!`), so a fresh DB is set up on first successful deploy — no manual migration step.

8. **Custom domain certs have separate DNS and provider gates.** For `api.murmurnotes.io`, Cloudflare
   needs both CNAME and TXT verification, DNS-only. `railway domain certificate retry` only works
   after issuance fails; while Railway says `VALIDATING_OWNERSHIP`, wait and poll. Do not delete and
   re-add the custom domain unless forced, because Railway may assign a new CNAME target.

## Deploy-from-scratch (if the project is ever recreated)

1. Create project → capture project id + production environment id (`projectCreate`).
2. Add a **managed Postgres** (dashboard "Add PostgreSQL", or a Postgres template deploy).
3. Create the server service (`serviceCreate` name `murmur-server`) → set `DATABASE_URL=${{Postgres.DATABASE_URL}}`, `RUST_LOG=info`.
4. Create a project token (`projectTokenCreate`) → `railway up --service murmur-server` from the repo.
5. `customDomainCreate` or `railway domain api.murmurnotes.io` → add Cloudflare CNAME/TXT → wait for
   `verified=true` + cert `VALID` → set `SHARE_BASE_URL=https://api.murmurnotes.io` → `railway up`
   again to apply.
6. Verify `/healthz` + a blob round-trip + the restore posture.

## What's NOT done yet (for a real public launch — see docs/research)

The live instance is still a **test/demo deploy**, now with the branded API domain and working CI/CD.
For a real public launch (per `docs/research/2026-07-04-murmur-server-hosting-decision.md` +
`agent-managed-hosting.md`): settle the abuse/DSA/legal posture, invite-only scoping to shrink the
abuse surface, backups + a restore drill, monitoring/alerts, and the go/no-go on
solo-managed-vs-abuse-desk. Managing the *tech* ops loop (deploy/monitor/backup/restore) is fully
agent-doable; the legal/abuse residue needs a reachable human. The strategic pick if this becomes the
permanent home is Hetzner-EU + the verbatim compose (this Railway deploy trades the
self-host-artifact property for one-command ease).
