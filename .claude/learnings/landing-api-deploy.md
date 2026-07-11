# Landing + API Deploy Runbook

Current public surfaces:

- Landing: `https://murmurnotes.io/`
- Landing `www`: `https://www.murmurnotes.io/` redirects to apex.
- API: `https://api.murmurnotes.io/healthz`
- API fallback: `https://murmur-server-production-b9e8.up.railway.app/healthz`

## What Was Shipped

Landing:

- Repo: `murmur-io/murmur`, trunk branch `murmur`.
- Static source: `landing/`.
- Required artifact file: `landing/CNAME` with exactly `murmurnotes.io`.
- Workflow: `.github/workflows/pages.yml` (`Deploy landing to Pages`), triggered on pushes to
  `landing/**` or the workflow file.
- GitHub Pages custom domain: `murmurnotes.io`.
- Final GitHub Pages state: `https_enforced=true`, `html_url=https://murmurnotes.io/`.
- PR that fixed cert issuance: `murmur-io/murmur#216` (`Add Pages custom domain CNAME`).

API:

- Repo: `murmur-io/murmur-server`, branch `main`.
- Host: Railway project `27e7b5a4-b5db-40ad-a63a-2a4d8276b29b`, env
  `8ef5a21d-96c6-4141-80ed-be4b7fc2f855`, service `murmur-server`.
- Service id: `4d55d97c-1a99-4a51-a947-657a012a9cfc`.
- Managed Postgres service id: `570954b2-32bc-42f4-b6c1-80d76959951d`.
- Custom domain id: `c225ac0a-0494-4682-b459-cf5c8e234bd4`.
- `SHARE_BASE_URL=https://api.murmurnotes.io`.
- Final Railway domain state: `verification.verified=true`,
  `certificate.status=CERTIFICATE_STATUS_TYPE_VALID`.
- PR that fixed CI/CD and deploy hygiene: `murmur-io/murmur-server#8`.

## Cloudflare DNS

Keep all these records **DNS only** unless deliberately switching to Cloudflare proxy.

Landing apex:

- `A @ 185.199.108.153`
- `A @ 185.199.109.153`
- `A @ 185.199.110.153`
- `A @ 185.199.111.153`

Landing www:

- `CNAME www murmur-io.github.io`

API:

- `CNAME api k9sfnbwk.up.railway.app`
- `TXT _railway-verify.api railway-verify=f5bf42c0232021570fa6a25f0d7906dcda7992b5d130b8d7ddecdda1a228445b`

Email records (`send`, `resend._domainkey`, SPF/MX) are separate and should not be changed for the
landing/API flow.

## CI/CD Flow

Landing:

1. Edit files under `landing/`.
2. Ensure `landing/CNAME` remains present.
3. Open PR to `murmur`.
4. Merge via PR, never direct-push trunk.
5. `Deploy landing to Pages` publishes the `landing` artifact to GitHub Pages.
6. Verify HTTPS and cert after deploy.

API:

1. Work in `/Users/jakubgawronski/Projects/murmur-server` or a clean worktree.
2. Open PR to `murmur-io/murmur-server:main`.
3. CI workflow `ci` runs `gate`, `supply-chain`, and `docker-build`.
4. On green `ci` on `main`, workflow `deploy` runs `npx @railway/cli@5.23.3 up --ci` with the
   GitHub secret `RAILWAY_TOKEN`.
5. For env variable changes, trigger `deploy` manually after `variableUpsert`; Railway runtime only
   sees env changes after a fresh `railway up` deployment.

Branch protection note: GitHub branch protection was unavailable for the private `murmur-server`
repo on the current plan (`Upgrade to GitHub Pro or make this repository public`). CI exists and
deploy is gated by green `ci`, but GitHub cannot enforce required PR checks until the plan/repo
visibility changes.

## Verification Commands

Landing DNS:

```bash
dig +short murmurnotes.io A @1.1.1.1
dig +short www.murmurnotes.io CNAME @1.1.1.1
dig +short murmurnotes.io CAA @1.1.1.1
```

Landing Pages and cert:

```bash
gh api repos/murmur-io/murmur/pages --jq '{cname,html_url,https_enforced,pending_domain_unverified_at}'
curl -I --max-time 15 https://murmurnotes.io/
curl -I --max-time 15 https://www.murmurnotes.io/
echo | openssl s_client -servername murmurnotes.io -connect murmurnotes.io:443 2>/dev/null \
  | openssl x509 -noout -subject -issuer -text \
  | rg -A2 'Subject:|Issuer:|DNS:'
```

Expected landing cert:

- Subject: `CN=murmurnotes.io`
- SAN: `DNS:murmurnotes.io`, `DNS:www.murmurnotes.io`
- Pages: `https_enforced=true`

API DNS:

```bash
dig +short api.murmurnotes.io CNAME @1.1.1.1
dig +short _railway-verify.api.murmurnotes.io TXT @1.1.1.1
```

API Railway domain:

```bash
RAILWAY_TOKEN="$(cat ~/.murmur/railway-projtoken)" \
npx --yes @railway/cli@5.23.3 domain status api.murmurnotes.io \
  --project 27e7b5a4-b5db-40ad-a63a-2a4d8276b29b \
  --environment 8ef5a21d-96c6-4141-80ed-be4b7fc2f855 \
  --service murmur-server \
  --json | jq '{verified:.domain.verification.verified, cert:.domain.certificate.status, current:.domain.dnsRecords[0].currentValue}'
```

API health:

```bash
curl -fsS --max-time 20 https://api.murmurnotes.io/healthz
```

Expected API result:

```json
{"status":"ok","version":"0.1.0"}
```

## Cert Failure Playbook

Railway API cert:

- If `currentValue` is wrong, fix Cloudflare `api` CNAME to `k9sfnbwk.up.railway.app`.
- If `verification.verified=false`, add/check `_railway-verify.api` TXT.
- `railway domain certificate retry` only works after issuance fails; while status is
  `VALIDATING_OWNERSHIP`, wait and poll.
- Do not delete/re-add the Railway custom domain unless necessary; it can change the required CNAME
  target and force another DNS edit.

GitHub Pages landing cert:

- If Chrome says `NET::ERR_CERT_COMMON_NAME_INVALID`, first check whether GitHub is still serving
  `CN=*.github.io`.
- Ensure `landing/CNAME` is included in the Pages artifact.
- Ensure `murmurnotes.io` apex uses the four GitHub Pages A records and no conflicting AAAA/CAA.
- If `gh api -X PUT ... {"https_enforced":true}` says `The certificate does not exist yet`, remove
  and re-add the Pages custom domain to retrigger issuance:

```bash
gh api -X PUT repos/murmur-io/murmur/pages --input - <<'JSON'
{"cname":null}
JSON
sleep 3
gh api -X PUT repos/murmur-io/murmur/pages --input - <<'JSON'
{"cname":"murmurnotes.io"}
JSON
gh api repos/murmur-io/murmur/pages/health --include
```

- Then poll enabling HTTPS. The intermediate good sign is `The certificate has not finished being
  issued`; after issuance succeeds, `https://murmurnotes.io/` returns `HTTP/2 200` and
  `https_enforced=true`.

