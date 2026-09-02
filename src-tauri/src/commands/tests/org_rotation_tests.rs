//! Oracles for the org key rotation that follows a member removal.
//!
//! Removing somebody from a Shared Brain is two operations wearing one button: drop their
//! membership, and rotate the org content key so nothing published from then on is readable with
//! the key they still hold. On trunk the second half had never once run.
//!
//! Three independent defects, each sufficient on its own:
//!
//! 1. `share::client::org_bump_generation` POSTed `/v1/orgs/{id}/generation` with **no body**. The
//!    server extracts it with axum's `Json<BumpGenerationRequest>`, which rejects a request with no
//!    `Content-Type: application/json` at **415, before the handler runs** — so the call never even
//!    reached the owner check.
//! 2. Even with a body, the client prepared a grant for the OWNER ONLY, while the relay commits a
//!    generation only when a grant for it exists for EVERY active member, checked inside the same
//!    transaction. An owner-only rotation is not a partial rotation; it is a 409 and no rotation.
//! 3. The rotation ran after the removal with no journal, so any interruption — a dropped
//!    connection, a quit — left the org permanently on the generation the removed member could
//!    open, with nothing to re-drive it.
//!
//! The fixture below therefore does NOT just record calls. `RotationRelay` implements the relay's
//! real coverage rule: a bump whose grants miss any active member is refused with 409, exactly as
//! Postgres does it. Without that, "we called PUT key-grants" would pass for an owner-only rotation
//! and the oracle would be decorative. `the_relay_fixture_refuses_a_bump_that_misses_a_member` is
//! the control that proves the rule in the fixture is real, so this guard cannot go vacuous.

use super::*;
use crate::error::AppError;
use crate::storage::db::Db;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// The fixed 64-hex dev key every test DB in this crate opens with, BUILT rather than written out
/// so this file carries no literal in DEK/KEK shape.
fn test_dek() -> String {
    "0123456789abcdef".repeat(4)
}

fn fresh_db(label: &str) -> Db {
    let path = crate::storage::db::unique_temp_path(&format!("murmur-orgrot-{label}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    Db::open_with_key(&path, &test_dek()).unwrap()
}

const ORG: &str = "8f14e45f-ea6f-4b6d-9c1a-2f2b1c0d3e40";
const OWNER_UID: &str = "c534b6d2-02c1-4c2c-a256-3af8592b1567";
const STAYS_CACHED_UID: &str = "11111111-2222-3333-4444-555555555555";
const STAYS_LOOKED_UP_UID: &str = "66666666-7777-8888-9999-000000000000";
const REMOVED_UID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// A member's published identity, derived the same way a real account's is, so the OCK wrap that
/// rotation performs is exercised against a genuine X25519 key rather than arbitrary bytes.
fn member_identity(seed: u8, account: &str) -> crate::e2ee::keys::IdentityKeypair {
    crate::e2ee::keys::derive_identity(&Zeroizing::new([seed; 32]), account, 1).unwrap()
}

fn seed_live_session(state: &AppState) {
    *state.account_session.lock().unwrap() = Some(crate::share::AccountSession {
        account_id: "owner@example.com".into(),
        email: "owner@example.com".into(),
        server_user_id: Some(OWNER_UID.into()),
        device_id: "dev-1".into(),
        mk: Zeroizing::new([7u8; 32]),
        generation: 1,
        access_token: "a".into(),
        access_expires_at: Some("2099-01-01T00:00:00Z".into()),
        refresh_token: "r".into(),
    });
}

fn seed_org(db: &Db, generation: u32) {
    db.upsert_org_state(&crate::storage::OrgState {
        org_id: ORG.into(),
        name: "Acme".into(),
        role: "owner".into(),
        joined_at: "2026-07-11T00:00:00Z".into(),
        consented: true,
        last_seq: 0,
        generation,
        context_enabled: true,
    })
    .unwrap();
}

// ── The relay fixture ────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct MockMember {
    user_id: String,
    role: String,
    email: Option<String>,
}

#[derive(Default)]
struct RelayLog {
    /// `METHOD /path`, in the order the client issued them — the only place request ORDER is
    /// observable, and ordering is half of what this change is about.
    requests: Vec<String>,
    /// The `Content-Type` the generation bump carried, if any. `None` here is defect 1 exactly.
    bump_content_type: Option<String>,
    bump_body: Option<String>,
    /// Every `(user_id, generation)` a key-grant PUT covered, across all calls.
    granted: Vec<(String, u32)>,
    removed: Vec<String>,
    lookups: Vec<String>,
    /// The bump the relay actually committed, if it committed one.
    committed_generation: Option<u32>,
    /// 409 the next bump regardless of coverage — models a relay that refuses for its own reasons.
    refuse_bump: bool,
}

struct RotationRelay {
    base: String,
    log: Arc<Mutex<RelayLog>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RotationRelay {
    /// `members` is the live roster; `keys` maps an email to the identity the lookup publishes.
    fn start(members: Vec<MockMember>, keys: HashMap<String, (Vec<u8>, Vec<u8>)>) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", server.server_addr().to_ip().unwrap());
        let log = Arc::new(Mutex::new(RelayLog::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let log_t = Arc::clone(&log);
        let shutdown_t = Arc::clone(&shutdown);

        let handle = std::thread::spawn(move || {
            // The relay's own state: the roster and the live generation it would keep in Postgres.
            let mut roster = members;
            let mut generation: u32 = 1;
            loop {
                if shutdown_t.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(Some(mut req)) = server.recv_timeout(std::time::Duration::from_millis(50))
                else {
                    continue;
                };
                let method = req.method().clone();
                let url = req.url().to_string();
                let path = url.split('?').next().unwrap_or("").to_string();
                let content_type = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Content-Type"))
                    .map(|h| h.value.as_str().to_string());
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                log_t
                    .lock()
                    .unwrap()
                    .requests
                    .push(format!("{method} {path}"));

                let json = |b: String| {
                    tiny_http::Response::from_string(b).with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    )
                };

                // GET /v1/orgs/{id} — the caller's view, including the LIVE generation.
                if method == tiny_http::Method::Get && path == format!("/v1/orgs/{ORG}") {
                    let _ = req.respond(json(format!(
                        "{{\"orgId\":\"{ORG}\",\"name\":\"Acme\",\"role\":\"owner\",\
                          \"createdAt\":\"2026-07-11T00:00:00Z\",\"currentGeneration\":{generation}}}"
                    )));
                    continue;
                }

                // GET /v1/orgs/{id}/members — ACTIVE members only, as the real route does.
                if method == tiny_http::Method::Get && path == format!("/v1/orgs/{ORG}/members") {
                    let rows = roster
                        .iter()
                        .map(|m| {
                            let email = match &m.email {
                                Some(e) => format!(",\"email\":\"{e}\""),
                                None => String::new(),
                            };
                            format!(
                                "{{\"userId\":\"{}\",\"role\":\"{}\",\
                                  \"createdAt\":\"2026-07-11T00:00:00Z\"{}}}",
                                m.user_id, m.role, email
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    let _ = req.respond(json(format!("{{\"members\":[{rows}]}}")));
                    continue;
                }

                // DELETE /v1/orgs/{id}/members/{uid} — drops them from the ACTIVE roster, which is
                // what makes the coverage rule below stop counting them.
                if method == tiny_http::Method::Delete
                    && path.starts_with(&format!("/v1/orgs/{ORG}/members/"))
                {
                    let uid = path.rsplit('/').next().unwrap_or("").to_string();
                    roster.retain(|m| m.user_id != uid);
                    log_t.lock().unwrap().removed.push(uid);
                    let _ = req.respond(tiny_http::Response::empty(200));
                    continue;
                }

                // POST /v1/keys/lookup {email}
                if method == tiny_http::Method::Post && path == "/v1/keys/lookup" {
                    let email = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| {
                            v.get("email")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    log_t.lock().unwrap().lookups.push(email.clone());
                    let reply = match keys.get(&email) {
                        Some((pk_enc, pk_sig)) => format!(
                            "{{\"registered\":true,\"key\":{{\"userId\":\"{}\",\"generation\":1,\
                              \"pkEnc\":\"{}\",\"pkSig\":\"{}\",\"fingerprint\":\"fp-{}\"}}}}",
                            roster
                                .iter()
                                .find(|m| m.email.as_deref() == Some(email.as_str()))
                                .map(|m| m.user_id.clone())
                                .unwrap_or_default(),
                            murmur_protocol::b64::encode(pk_enc),
                            murmur_protocol::b64::encode(pk_sig),
                            email
                        ),
                        None => "{\"registered\":false}".to_string(),
                    };
                    let _ = req.respond(json(reply));
                    continue;
                }

                // PUT /v1/orgs/{id}/key-grants — opaque bytes, recorded by (user, generation).
                if method == tiny_http::Method::Put && path == format!("/v1/orgs/{ORG}/key-grants")
                {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(grants) = v.get("grants").and_then(serde_json::Value::as_array) {
                            let mut guard = log_t.lock().unwrap();
                            for g in grants {
                                let uid = g
                                    .get("userId")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                let gen = g
                                    .get("generation")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0) as u32;
                                guard.granted.push((uid, gen));
                            }
                        }
                    }
                    let _ = req.respond(tiny_http::Response::empty(200));
                    continue;
                }

                // POST /v1/orgs/{id}/generation {generation} — the whole point.
                if method == tiny_http::Method::Post && path == format!("/v1/orgs/{ORG}/generation")
                {
                    {
                        let mut guard = log_t.lock().unwrap();
                        guard.bump_content_type = content_type.clone();
                        guard.bump_body = Some(body.clone());
                    }
                    // Defect 1, modelled exactly: axum's `Json` extractor rejects a request that
                    // does not declare JSON with 415 BEFORE the handler runs.
                    if !content_type
                        .as_deref()
                        .is_some_and(|c| c.starts_with("application/json"))
                    {
                        let _ = req.respond(tiny_http::Response::empty(415));
                        continue;
                    }
                    let Some(requested) = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("generation").and_then(serde_json::Value::as_u64))
                        .map(|g| g as u32)
                    else {
                        let _ = req.respond(tiny_http::Response::empty(422));
                        continue;
                    };
                    let refuse = log_t.lock().unwrap().refuse_bump;
                    // Monotonic-by-one, then FULL COVERAGE — the two conditions
                    // `store::orgs::bump_generation` checks in one transaction.
                    let covered: HashSet<String> = log_t
                        .lock()
                        .unwrap()
                        .granted
                        .iter()
                        .filter(|(_, g)| *g == requested)
                        .map(|(u, _)| u.clone())
                        .collect();
                    let all_covered = !roster.is_empty()
                        && roster.iter().all(|m| covered.contains(&m.user_id));
                    if refuse || requested != generation + 1 || !all_covered {
                        let _ = req.respond(tiny_http::Response::empty(409));
                        continue;
                    }
                    generation = requested;
                    log_t.lock().unwrap().committed_generation = Some(generation);
                    let _ = req.respond(json(format!(
                        "{{\"currentGeneration\":{generation}}}"
                    )));
                    continue;
                }

                let _ = req.respond(tiny_http::Response::empty(404));
            }
        });

        Self {
            base,
            log,
            shutdown,
            handle: Some(handle),
        }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, RelayLog> {
        self.log.lock().unwrap()
    }

    fn set_refuse_bump(&self, refuse: bool) {
        self.log.lock().unwrap().refuse_bump = refuse;
    }

    /// Every user id granted at `generation`, deduplicated and sorted — the set the coverage rule
    /// cares about.
    fn granted_at(&self, generation: u32) -> Vec<String> {
        let mut v: Vec<String> = self
            .log()
            .granted
            .iter()
            .filter(|(_, g)| *g == generation)
            .map(|(u, _)| u.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

impl Drop for RotationRelay {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// One owner + three members; the removal target plus two who stay — one whose key this device
/// already remembers, one it must look up.
fn roster() -> Vec<MockMember> {
    vec![
        MockMember {
            user_id: OWNER_UID.into(),
            role: "owner".into(),
            email: Some("owner@example.com".into()),
        },
        MockMember {
            user_id: STAYS_CACHED_UID.into(),
            role: "member".into(),
            email: Some("cached@example.com".into()),
        },
        MockMember {
            user_id: STAYS_LOOKED_UP_UID.into(),
            role: "member".into(),
            email: Some("lookedup@example.com".into()),
        },
        MockMember {
            user_id: REMOVED_UID.into(),
            role: "member".into(),
            email: Some("removed@example.com".into()),
        },
    ]
}

fn published_keys() -> HashMap<String, (Vec<u8>, Vec<u8>)> {
    let mut keys = HashMap::new();
    for (seed, email) in [
        (3u8, "cached@example.com"),
        (4u8, "lookedup@example.com"),
        (5u8, "removed@example.com"),
    ] {
        let id = member_identity(seed, email);
        keys.insert(email.to_string(), (id.pk_enc.to_vec(), id.pk_sig.to_vec()));
    }
    keys
}

fn wire_state(state: &AppState, relay: &RotationRelay) {
    seed_live_session(state);
    seed_org(&state.db, 1);
    state.config.lock().unwrap().share_base_url = relay.base.clone();
}

/// Remember the "cached" member's key, as an invite would have.
fn seed_cached_member_key(state: &AppState) {
    let id = member_identity(3, "cached@example.com");
    let fp = crate::e2ee::key_fingerprint(&id.pk_enc, &id.pk_sig);
    state
        .db
        .upsert_org_member_key(
            ORG,
            STAYS_CACHED_UID,
            Some("cached@example.com"),
            &id.pk_enc,
            &id.pk_sig,
            &fp,
        )
        .unwrap();
}

// ── The oracles ──────────────────────────────────────────────────────────────────────────────────

/// THE headline oracle. RED on trunk three times over: the bump carried no `Content-Type` (415),
/// the grants covered only the owner (409 on coverage), and nothing recorded the debt.
#[tokio::test]
async fn removing_a_member_rotates_the_key_for_every_remaining_member() {
    let relay = RotationRelay::start(roster(), published_keys());
    let state = AppState::for_tests(fresh_db("rotate-covers-all"));
    wire_state(&state, &relay);
    seed_cached_member_key(&state);

    org_remove_member_inner(&state, ORG.into(), REMOVED_UID.into())
        .await
        .expect("removal + rotation must succeed");

    // 1. The bump is a real JSON request the server's extractor accepts, carrying gen 2.
    assert_eq!(
        relay.log().bump_content_type.as_deref().map(|c| c
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()),
        Some("application/json".to_string()),
        "the generation bump must declare JSON — without it axum answers 415 before the handler"
    );
    assert_eq!(
        relay.log().bump_body.as_deref(),
        Some("{\"generation\":2}"),
        "the bump must name the generation it is committing"
    );

    // 2. Coverage: every REMAINING member, and nobody who was removed.
    assert_eq!(
        relay.granted_at(2),
        {
            let mut expected = vec![
                OWNER_UID.to_string(),
                STAYS_CACHED_UID.to_string(),
                STAYS_LOOKED_UP_UID.to_string(),
            ];
            expected.sort();
            expected
        },
        "gen 2 must be wrapped for every remaining member (owner-only is what the relay 409s)"
    );
    assert!(
        !relay
            .granted_at(2)
            .iter()
            .any(|u| u == REMOVED_UID),
        "the removed member must not be granted the new key"
    );

    // 3. The relay committed it, and the local view agrees.
    assert_eq!(relay.log().committed_generation, Some(2));
    assert_eq!(state.db.get_org_state(ORG).unwrap().unwrap().generation, 2);

    // 4. Order: the member is out, then the grants land, then the generation flips. Granting before
    //    the removal would mean granting the new key to the person being removed.
    let requests = relay.log().requests.clone();
    let pos = |needle: &str| requests.iter().position(|r| r.contains(needle)).unwrap();
    assert!(
        pos("DELETE /v1/orgs") < pos("PUT /v1/orgs") && pos("PUT /v1/orgs") < pos("POST /v1/orgs"),
        "expected remove → grants → bump, got {requests:?}"
    );

    // 5. Only the member whose key was NOT remembered cost a lookup — the quota is 20/day against
    //    orgs of up to 50, so re-looking-up everybody would fail on quota, not on correctness.
    assert_eq!(
        relay.log().lookups,
        vec!["lookedup@example.com".to_string()],
        "a remembered key must not be re-fetched"
    );

    // 6. Debt settled, and the departing member's key is forgotten.
    assert_eq!(state.db.org_rotation_pending_attempts(ORG).unwrap(), None);
    assert!(state
        .db
        .get_org_member_key(ORG, REMOVED_UID)
        .unwrap()
        .is_none());
}

/// A rotation that cannot finish must leave a debt that a later sweep settles — the window between
/// "removed" and "rotated" is exactly when new posts are still readable with the old key.
#[tokio::test]
async fn an_unfinished_rotation_leaves_a_debt_a_later_sweep_settles() {
    let relay = RotationRelay::start(roster(), published_keys());
    let state = AppState::for_tests(fresh_db("rotate-redrive"));
    wire_state(&state, &relay);
    seed_cached_member_key(&state);
    relay.set_refuse_bump(true);

    let err = org_remove_member_inner(&state, ORG.into(), REMOVED_UID.into())
        .await
        .expect_err("a refused bump must be reported, not swallowed");
    assert!(
        matches!(&err, AppError::Unavailable(m) if m.contains(crate::errcode::ORG_ROTATION_PENDING)),
        "the failure must carry the rotation-pending code, not read as a failed removal: {err}"
    );

    // The member IS gone; the generation is NOT rotated; the debt is recorded.
    assert_eq!(relay.log().removed, vec![REMOVED_UID.to_string()]);
    assert_eq!(
        state.db.get_org_state(ORG).unwrap().unwrap().generation,
        1,
        "no generation may be recorded locally that the relay did not commit"
    );
    assert!(
        state
            .db
            .org_rotation_pending_attempts(ORG)
            .unwrap()
            .is_some_and(|n| n >= 1),
        "the debt must survive with its attempt recorded"
    );

    // The relay recovers; the retry closes the window without any user action.
    relay.set_refuse_bump(false);
    let settled = drive_pending_org_rotations(&state)
        .await
        .unwrap();
    assert_eq!(settled, 1);
    assert_eq!(relay.log().committed_generation, Some(2));
    assert_eq!(state.db.get_org_state(ORG).unwrap().unwrap().generation, 2);
    assert_eq!(state.db.org_rotation_pending_attempts(ORG).unwrap(), None);
}

/// THE CONTROL. If the fixture did not enforce the relay's coverage rule, the oracle above would
/// pass for an owner-only rotation — which is precisely the shipped bug. This asserts the rule in
/// the fixture is real, so the guard cannot quietly go vacuous.
#[tokio::test]
async fn the_relay_fixture_refuses_a_bump_that_misses_a_member() {
    let relay = RotationRelay::start(roster(), published_keys());
    let client = crate::share::client::ShareClient::new(&relay.base).unwrap();

    // Grant gen 2 to the OWNER ONLY — exactly what trunk sent.
    client
        .org_put_key_grants(
            "a",
            ORG,
            vec![crate::share::org_dto::KeyGrantInput {
                user_id: OWNER_UID.into(),
                generation: 2,
                wrapped_key: vec![1, 2, 3],
                grant_sig: vec![4, 5, 6],
            }],
        )
        .await
        .unwrap();

    let err = client
        .org_bump_generation("a", ORG, 2)
        .await
        .expect_err("an owner-only rotation must be refused");
    // A 409 is a REFUSAL of the request, and the client maps it to the tagged `sharing-rejected`
    // shape the frontend already has copy for. What matters to this control is that the fixture
    // said 409 at all — the variant is the client's mapping, not the rule under test.
    assert!(
        err.to_string().contains("409"),
        "expected the coverage refusal the real relay gives, got {err}"
    );
    assert_eq!(relay.log().committed_generation, None);
}

/// The journal is written BEFORE the removal call, so an interruption between the two still leaves
/// a re-drivable debt. Asserted by driving a removal against a relay that answers nothing: the
/// removal fails, and the debt is still on the books.
#[tokio::test]
async fn the_rotation_debt_is_journaled_before_the_member_is_removed() {
    let state = AppState::for_tests(fresh_db("rotate-journal-first"));
    seed_live_session(&state);
    seed_org(&state.db, 1);
    // A port nothing listens on: every request fails, including the removal.
    state.config.lock().unwrap().share_base_url = "http://127.0.0.1:1/".into();

    let _ = org_remove_member_inner(&state, ORG.into(), REMOVED_UID.into())
        .await;

    assert!(
        state
            .db
            .org_rotation_pending_attempts(ORG)
            .unwrap()
            .is_some(),
        "the debt must be recorded before the removal is attempted — a lost response is \
         indistinguishable from a success, and a redundant rotation is far cheaper than a skipped one"
    );
}

/// The member-key cache stores PUBLIC material and is keyed per org, so one org's view of a member
/// cannot answer for another's.
#[test]
fn remembered_member_keys_are_scoped_to_their_org() {
    let db = fresh_db("member-key-scope");
    let id = member_identity(3, "cached@example.com");
    let fp = crate::e2ee::key_fingerprint(&id.pk_enc, &id.pk_sig);
    db.upsert_org_member_key(
        ORG,
        STAYS_CACHED_UID,
        Some("cached@example.com"),
        &id.pk_enc,
        &id.pk_sig,
        &fp,
    )
    .unwrap();

    let here = db.get_org_member_key(ORG, STAYS_CACHED_UID).unwrap();
    assert_eq!(here.as_ref().map(|k| k.fingerprint.clone()), Some(fp));
    assert_eq!(here.map(|k| k.pk_enc), Some(id.pk_enc.to_vec()));
    assert!(
        db.get_org_member_key("other-org", STAYS_CACHED_UID)
            .unwrap()
            .is_none(),
        "a key learned in one org must not answer for another"
    );

    db.forget_org_member_key(ORG, STAYS_CACHED_UID).unwrap();
    assert!(db
        .get_org_member_key(ORG, STAYS_CACHED_UID)
        .unwrap()
        .is_none());
}
