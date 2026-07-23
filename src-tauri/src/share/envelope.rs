//! The "clean text" transform (spec §7 inv. 3, M-1/T3.2) — the ONE place a note's markdown is
//! sanitized before it enters a [`ShareEnvelope`] and leaves the device.
//!
//! This is PURE (no I/O, no state, no egress): it takes a note's raw markdown and returns a body
//! safe to hand to a non-Obsidian reader — YAML frontmatter removed, `[[wikilinks]]` flattened to
//! their display text, and `obsidian://` deep-links stripped. It is TDD'd first (RED before GREEN)
//! because this is exactly the `vault-titles-egress-leak` class: a stray `[[Alice Smith]]` or a
//! frontmatter `attendees:`/`aliases:` line that survived into the shared payload is a real leak of
//! other people's names / private vault structure. The transform NEVER touches the DB or the
//! network; the CALLER (`share_note_to_link`) is responsible for the `meeting_is_unlocked` gate that
//! decides whether it is even allowed to read the note in the first place.

/// Strip a leading YAML frontmatter block from a note's markdown.
///
/// Obsidian/Murmur notes begin with a `---` fence carrying keys like `attendees:`, `participants:`,
/// `aliases:`, `ai-provider:` — none of which belong in a shared body (real people's names leak
/// otherwise). Returns an OWNED `String` because we must first normalize line endings.
///
/// LEAK-CRITICAL fence tolerance (the class the adversarial verifier caught): the summarizer's own
/// `starts_with_frontmatter` accepts an opening fence line that is `---` AFTER `trim_end()` — i.e. a
/// TRAILING-SPACE fence `---  \n` and a CRLF fence `---\r\n` are BOTH valid frontmatter to the
/// providers, so a note with `participants:` under such a fence is persisted with it. This stripper
/// MUST match the same tolerant shape or those names egress. We therefore: (1) normalize CRLF→LF, and
/// (2) treat the block as frontmatter iff the FIRST line trims to exactly `---` (not `----`), then
/// remove through the matching closing `---` fence line. If no closing fence exists (malformed), we
/// fail SAFE by dropping every line up to EOF (the whole thing was frontmatter-shaped), never leaking
/// the unterminated block. A `---` that is NOT the very first line is a normal horizontal rule and is
/// left intact.
pub fn strip_frontmatter(markdown: &str) -> String {
    // Normalize line endings so a CRLF fence is recognized. (Bare `\r` old-Mac endings are not a
    // Murmur output shape; LF + CRLF cover every real note.)
    let normalized = markdown.replace("\r\n", "\n");

    let mut lines = normalized.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return normalized;
    };
    // Opening fence: the first line trims to exactly `---` (tolerates trailing spaces/tabs), matching
    // `summarize::…::starts_with_frontmatter`. `----` or `--- text` is NOT a fence.
    if first.trim_end() != "---" {
        return normalized;
    }

    // Walk the remaining lines to the FIRST line that trims to exactly `---` (the close fence); return
    // everything after it, with leading blank lines trimmed. No close fence ⇒ fail safe: return "".
    let mut consumed = first.len();
    for line in lines {
        consumed += line.len();
        if line.trim_end() == "---" {
            let body = &normalized[consumed..];
            return body.trim_start_matches('\n').to_string();
        }
    }
    // Unterminated frontmatter-shaped block ⇒ drop it entirely (never leak attendee names).
    String::new()
}

/// Flatten Obsidian `[[wikilinks]]` to plain display text and strip `obsidian://` deep-links.
///
/// - `[[Target]]`            → `Target`
/// - `[[Target|Alias]]`      → `Alias` (the alias is what a human reads)
/// - `[[Target#Heading]]`    → `Target#Heading` → we keep the visible `Target#Heading` text minus
///   the block/heading marker where an alias is absent (kept simple: drop only the `[[` `]]`)
/// - `obsidian://…` URIs     → removed entirely (they only resolve inside the author's vault and can
///   embed the vault name + a note path = a structure leak).
///
/// Embedded transclusions (`![[…]]`) are flattened the same way as `[[…]]` (the leading `!` is
/// dropped so it renders as inline text, never an image/embed directive).
pub fn flatten_wikilinks(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let bytes = markdown.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // `![[ … ]]` (embed) — treat as a normal wikilink, dropping the leading `!`.
        if bytes[i] == b'!' && markdown[i..].starts_with("![[") {
            if let Some(rel_end) = markdown[i + 3..].find("]]") {
                let inner = &markdown[i + 3..i + 3 + rel_end];
                out.push_str(&flatten_link_inner(inner));
                i = i + 3 + rel_end + 2;
                continue;
            }
        }
        // `[[ … ]]` (wikilink).
        if markdown[i..].starts_with("[[") {
            if let Some(rel_end) = markdown[i + 2..].find("]]") {
                let inner = &markdown[i + 2..i + 2 + rel_end];
                out.push_str(&flatten_link_inner(inner));
                i = i + 2 + rel_end + 2;
                continue;
            }
        }
        // `obsidian://…` deep-link — strip up to the first whitespace or closing paren.
        if markdown[i..].starts_with("obsidian://") {
            let rest = &markdown[i..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ')')
                .unwrap_or(rest.len());
            i += end;
            continue;
        }
        // Default: copy this char.
        let ch = markdown[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Resolve the inner text of a `[[…]]` to what a human should read: the alias after `|` if present,
/// else the target (kept verbatim, sans the `[[` `]]`).
fn flatten_link_inner(inner: &str) -> String {
    match inner.split_once('|') {
        Some((_target, alias)) => alias.trim().to_string(),
        None => inner.trim().to_string(),
    }
}

/// The full "clean text" transform: strip frontmatter, DROP every machine-managed block, then flatten
/// wikilinks + strip `obsidian://` refs. This is the single scrub EVERY share path (link-share AND
/// every org-share) funnels a note's raw `notes.markdown` / `documents.text` through before building
/// the [`murmur_protocol::envelope::ShareEnvelope`]. Pure.
///
/// EGRESS HYGIENE — no MACHINE-MANAGED block may reach the share/org wire. Three fences carry the
/// sender's private data and are all HEADER-GATED-stripped here (each removed ONLY when its first
/// body line is its exact machine callout header, so a forged bare fence in USER prose is NEVER eaten
/// — the fence constants + header strings come from their owning modules, never hardcoded):
/// - `murmur:links` (`> [!related]- Related notes`) — the `[[Title]]` of every linked note/meeting;
///   without the strip `flatten_wikilinks` would turn those markers into readable TITLES that reach
///   the recipient (e.g. a private "Zwolnienia Q3"). This block is ALSO retired app-wide (A/B).
/// - `murmur:context` (`> [!context]- Live context`) — LIVE CONNECTOR data (Jira issue/status/URL,
///   Slack channel+snippet+permalink, web snippets) + the sender's workspace host URLs.
/// - `murmur:verify` (`> [!verify]- Source check`) — Jira issue keys + verdicts + workspace URLs.
///
/// SCOPE: this is the EGRESS-ONLY strip. `murmur:context` / `murmur:verify` are in-app FEATURES the
/// user opted into (ran Enrich / Verify) and STAY visible in the editor — the read/display path
/// (`get_note`) strips `murmur:links` only. We only stop context/verify LEAKING on share.
/// Belt-and-braces: closes the leak for EXISTING enriched/verified notes AND any new one.
pub fn clean_note_body(markdown: &str) -> String {
    let body = strip_frontmatter(markdown);
    let body = crate::enrich::strip_managed_links_block(&body);
    let body = crate::enrich::strip_managed_context_block(&body);
    let body = crate::enrich::strip_verify_block_for_egress(&body);
    flatten_wikilinks(&body)
}

/// Keep only exact, owner-validated Murmur image markers. Missing/foreign internal ids and every
/// external image URL become inert alt text, so sharing never triggers a tracking request and never
/// emits an unresolved private marker.
pub fn sanitize_share_images(
    markdown: &str,
    allowed_ids: &std::collections::HashSet<String>,
) -> String {
    let refs = image_references(markdown);
    if refs.is_empty() {
        return markdown.to_string();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0;
    for reference in refs {
        out.push_str(&markdown[cursor..reference.start]);
        let keep = reference
            .url
            .strip_prefix("murmur-attachment://")
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .is_some_and(|id| allowed_ids.contains(&id.to_ascii_lowercase()));
        if keep {
            out.push_str(&markdown[reference.start..reference.end]);
        } else if reference.alt.trim().is_empty() {
            out.push_str("[Image unavailable]");
        } else {
            out.push_str(reference.alt.trim());
        }
        cursor = reference.end;
    }
    out.push_str(&markdown[cursor..]);
    out
}

/// Rewrite authenticated wire ids to fresh local ids during ingest. Any marker not present in the
/// verified manifest becomes inert alt text. Fresh ids avoid collisions when the same shared image
/// is accepted twice or published into multiple org items on one device.
pub fn remap_share_images(
    markdown: &str,
    id_map: &std::collections::HashMap<String, String>,
) -> String {
    let refs = image_references(markdown);
    if refs.is_empty() {
        return markdown.to_string();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0;
    for reference in refs {
        out.push_str(&markdown[cursor..reference.start]);
        let replacement = reference
            .url
            .strip_prefix("murmur-attachment://")
            .and_then(|id| id_map.get(&id.to_ascii_lowercase()));
        if let Some(local_id) = replacement {
            out.push_str("![");
            out.push_str(reference.alt);
            out.push_str("](murmur-attachment://");
            out.push_str(local_id);
            out.push(')');
        } else if reference.alt.trim().is_empty() {
            out.push_str("[Image unavailable]");
        } else {
            out.push_str(reference.alt.trim());
        }
        cursor = reference.end;
    }
    out.push_str(&markdown[cursor..]);
    out
}

struct ImageReference<'a> {
    start: usize,
    end: usize,
    alt: &'a str,
    url: &'a str,
}

/// Minimal bounds-checked scanner for the image syntax Murmur itself writes: `![alt](url)`. It does
/// not attempt to be a general Markdown parser; malformed/nested constructs simply remain text.
fn image_references(markdown: &str) -> Vec<ImageReference<'_>> {
    let mut refs = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = markdown[cursor..].find("![") {
        let start = cursor + relative_start;
        let alt_start = start + 2;
        let Some(relative_alt_end) = markdown[alt_start..].find("](") else {
            break;
        };
        let alt_end = alt_start + relative_alt_end;
        if markdown[alt_start..alt_end].contains('\n')
            || markdown[alt_start..alt_end].contains('\r')
        {
            cursor = alt_start;
            continue;
        }
        let url_start = alt_end + 2;
        let Some(relative_url_end) = markdown[url_start..].find(')') else {
            break;
        };
        let url_end = url_start + relative_url_end;
        if markdown[url_start..url_end].contains('\n')
            || markdown[url_start..url_end].contains('\r')
        {
            cursor = url_start;
            continue;
        }
        refs.push(ImageReference {
            start,
            end: url_end + 1,
            alt: &markdown[alt_start..alt_end],
            url: &markdown[url_start..url_end],
        });
        cursor = url_end + 1;
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_image_refs_are_deduplicated_and_ignore_code() {
        let id = "11111111-1111-4111-8111-111111111111";
        let md = format!(
            "![one](murmur-attachment://{id})\n![again](murmur-attachment://{id})\n`![code](murmur-attachment://22222222-2222-4222-8222-222222222222)`\n![track](https://example.test/pixel.png)"
        );
        assert_eq!(
            crate::commands::referenced_attachment_ids(&md).expect("valid markers"),
            std::collections::HashSet::from([id.to_string()])
        );
    }

    #[test]
    fn share_image_sanitizer_keeps_only_owner_validated_ids() {
        let keep = "11111111-1111-4111-8111-111111111111";
        let missing = "22222222-2222-4222-8222-222222222222";
        let md = format!(
            "before ![kept](murmur-attachment://{keep}) ![missing](murmur-attachment://{missing}) ![tracker](https://example.test/p.png) after"
        );
        let allowed = std::collections::HashSet::from([keep.to_string()]);
        let clean = sanitize_share_images(&md, &allowed);
        assert!(clean.contains(&format!("![kept](murmur-attachment://{keep})")));
        assert!(!clean.contains(missing));
        assert!(!clean.contains("https://"));
        assert!(clean.contains("missing") && clean.contains("tracker"));
    }

    #[test]
    fn ingest_remaps_wire_ids_and_flattens_unmanifested_markers() {
        let wire = "11111111-1111-4111-8111-111111111111";
        let local = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let missing = "22222222-2222-4222-8222-222222222222";
        let md = format!(
            "![kept](murmur-attachment://{wire}) ![missing](murmur-attachment://{missing})"
        );
        let map = std::collections::HashMap::from([(wire.to_string(), local.to_string())]);
        let remapped = remap_share_images(&md, &map);
        assert!(remapped.contains(local));
        assert!(!remapped.contains(wire) && !remapped.contains(missing));
        assert!(remapped.contains("missing"));
    }

    #[test]
    fn strips_a_leading_frontmatter_block() {
        let md = "---\ntitle: Weekly Sync\nattendees:\n  - Alice\n---\n# Body\n\ntext here\n";
        assert_eq!(strip_frontmatter(md), "# Body\n\ntext here\n");
    }

    #[test]
    fn no_frontmatter_is_returned_unchanged() {
        let md = "# Just a heading\n\nno frontmatter\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn a_horizontal_rule_is_not_a_frontmatter_fence() {
        // A `---` NOT at the very start of the doc is a normal horizontal rule and must survive.
        let md = "# Heading\n\nbefore\n\n---\n\nafter\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn frontmatter_only_document_yields_empty_body() {
        let md = "---\nattendees:\n  - Bob\n---\n";
        assert_eq!(strip_frontmatter(md), "");
    }

    #[test]
    fn four_dashes_is_not_a_fence() {
        // `----` (a horizontal rule / setext-ish) is NOT frontmatter — leave it (mirrors the
        // summarizer's `starts_with_frontmatter`, which rejects `----`).
        let md = "----\ntitle: x\nbody\n";
        assert_eq!(strip_frontmatter(md), md);
    }

    /// RED-before-GREEN regression for the `vault-titles-egress-leak` the adversarial verifier found:
    /// a TRAILING-SPACE opening fence (`---  \n`) and a CRLF fence (`---\r\n`) are BOTH accepted as
    /// frontmatter by the summarizer (`starts_with_frontmatter`: `first.trim_end() == "---"`), so a
    /// note persisted with `participants:` under such a fence MUST still have it stripped here. On the
    /// old `strip_prefix("---\n")` stripper these assertions FAIL (the names leak into the body).
    #[test]
    fn strips_trailing_space_and_crlf_frontmatter_fences_no_attendee_leak() {
        // Trailing-space fence.
        let ts = "---  \ntitle: Board strategy\nparticipants:\n  - Alice Smith\n  - Bob Jones\n---\n# Body\n";
        let ts_clean = strip_frontmatter(ts);
        assert!(
            !ts_clean.contains("participants"),
            "trailing-space fence must be stripped: {ts_clean:?}"
        );
        assert!(
            !ts_clean.contains("Alice Smith"),
            "attendee name must not leak: {ts_clean:?}"
        );
        assert!(
            !ts_clean.contains("Bob Jones"),
            "attendee name must not leak: {ts_clean:?}"
        );
        assert_eq!(ts_clean, "# Body\n");

        // CRLF fence (both open and close).
        let crlf = "---\r\ntitle: x\nattendees:\r\n  - Carol Danvers\r\n---\r\n# Notes\r\n";
        let crlf_clean = strip_frontmatter(crlf);
        assert!(
            !crlf_clean.contains("attendees"),
            "CRLF fence must be stripped: {crlf_clean:?}"
        );
        assert!(
            !crlf_clean.contains("Carol Danvers"),
            "attendee name must not leak: {crlf_clean:?}"
        );

        // Full transform end-to-end: an attendee name under a trailing-space fence never survives.
        let clean = clean_note_body(ts);
        assert!(!clean.contains("Alice Smith") && !clean.contains("participants"));
    }

    /// An unterminated frontmatter-shaped opening fence must FAIL SAFE (drop everything), never leak
    /// the block that follows an open `---` with no close.
    #[test]
    fn unterminated_frontmatter_fails_safe_and_leaks_nothing() {
        let md = "---\nattendees:\n  - Dana Scully\n(no closing fence, EOF)\n";
        let clean = strip_frontmatter(md);
        assert!(
            !clean.contains("attendees"),
            "unterminated block must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("Dana Scully"),
            "unterminated attendee must not leak: {clean:?}"
        );
        assert_eq!(clean, "");
    }

    #[test]
    fn flattens_plain_and_aliased_wikilinks() {
        assert_eq!(
            flatten_wikilinks("see [[Alice Smith]] soon"),
            "see Alice Smith soon"
        );
        assert_eq!(
            flatten_wikilinks("ping [[Project X|the project]] today"),
            "ping the project today"
        );
    }

    #[test]
    fn flattens_embeds_and_strips_obsidian_uris() {
        assert_eq!(flatten_wikilinks("![[Diagram]] shown"), "Diagram shown");
        assert_eq!(
            flatten_wikilinks("open (obsidian://open?vault=Private&file=Alice) now"),
            "open () now"
        );
        assert_eq!(
            flatten_wikilinks("link obsidian://open?file=Secret trailing"),
            "link  trailing"
        );
    }

    #[test]
    fn leaves_ordinary_brackets_and_text_alone() {
        // A single `[bracket]` (markdown link text) is not a wikilink.
        assert_eq!(
            flatten_wikilinks("a [link](x) and text"),
            "a [link](x) and text"
        );
        // An unterminated `[[` is copied verbatim (never panics, never eats the rest).
        assert_eq!(flatten_wikilinks("dangling [[oops"), "dangling [[oops");
    }

    #[test]
    fn clean_note_body_is_the_full_leak_safe_transform() {
        // The `vault-titles-egress-leak` class, end to end: a note with frontmatter carrying other
        // people's names + a `[[Alice]]` wikilink + an obsidian:// ref → clean body with NONE of the
        // private vault structure.
        let md =
            "---\nattendees:\n  - Alice Smith\n  - Bob Jones\naliases: [secret-project]\n---\n\
                  # Decisions\n\n- talked with [[Alice Smith]] about [[Project X|the roadmap]]\n\
                  - ref: obsidian://open?vault=Work&file=Roadmap\n";
        let clean = clean_note_body(md);
        // Frontmatter (attendees / aliases) is gone.
        assert!(
            !clean.contains("attendees"),
            "frontmatter must be stripped: {clean:?}"
        );
        assert!(
            !clean.contains("aliases"),
            "frontmatter must be stripped: {clean:?}"
        );
        assert!(
            !clean.contains("Bob Jones"),
            "frontmatter attendee names must not leak: {clean:?}"
        );
        // Wikilinks are flattened to their display text; the vault target markers are gone.
        assert!(
            !clean.contains("[["),
            "wikilink markers must be flattened: {clean:?}"
        );
        assert!(
            clean.contains("Alice Smith"),
            "the visible display text is preserved: {clean:?}"
        );
        assert!(
            clean.contains("the roadmap"),
            "the alias is preserved: {clean:?}"
        );
        // obsidian:// deep-links (which embed the vault name) are removed.
        assert!(
            !clean.contains("obsidian://"),
            "obsidian:// refs must be stripped: {clean:?}"
        );
        assert!(
            !clean.contains("vault=Work"),
            "the vault name must not leak: {clean:?}"
        );
    }

    /// FIX 3 (CONTENT LEAK) — every share path funnels a note's RAW markdown through `clean_note_body`
    /// before building the envelope. A note carrying the machine `> [!related]- Related notes`
    /// (`murmur:links`) block lists the `[[Title]]` of every note/meeting it links to; the linked
    /// notes' CONTENT is never shared, but WITHOUT this strip `flatten_wikilinks` would turn those
    /// markers into plain readable TITLES that reach the recipient (a title-egress leak). The block —
    /// header, callout, and the linked title — must be entirely GONE from the shared body.
    /// RED before FIX 3: the block reached the envelope, "Related notes"/"Secret Note"/"(via note)"
    /// all leaked through.
    #[test]
    fn clean_note_body_strips_the_related_notes_block() {
        let md = "# Meeting\n\nReal prose the user wrote.\n\n\
                  <!-- murmur:links -->\n\
                  > [!related]- Related notes\n\
                  > - [[Secret Note]] (via note)\n\
                  <!-- /murmur:links -->\n";
        let clean = clean_note_body(md);
        assert!(
            clean.contains("Real prose the user wrote."),
            "the user's own prose survives the share scrub: {clean:?}"
        );
        assert!(
            !clean.contains("Related notes"),
            "the machine Related-notes callout must not reach the envelope: {clean:?}"
        );
        assert!(
            !clean.contains("Secret Note"),
            "a linked note TITLE must not leak to the share recipient: {clean:?}"
        );
        assert!(
            !clean.contains("(via note)"),
            "the machine block attribution must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("murmur:links") && !clean.contains("[!related]"),
            "no trace of the managed block reaches the shared body: {clean:?}"
        );
    }

    /// FIX 3 (generalized) — the `murmur:context` connector block carries LIVE Jira/Slack/web data +
    /// the sender's workspace host URLs. It must NOT reach the share envelope. HEADER-GATED on the
    /// `> [!context]- Live context` prefix (the callout also carries `(as of {date})`). RED before the
    /// generalization: the block + its Jira key + atlassian host URL leaked through.
    #[test]
    fn clean_note_body_strips_context_block() {
        let md = "# Meeting\n\nReal prose the user wrote.\n\n\
                  <!-- murmur:context -->\n\
                  > [!context]- Live context (as of 2026-07-05T14:32:00Z)\n\
                  > - PROJ-123 · In Progress (via Jira) — https://acme.atlassian.net/browse/PROJ-123\n\
                  <!-- /murmur:context -->\n";
        let clean = clean_note_body(md);
        assert!(
            clean.contains("Real prose the user wrote."),
            "the user's own prose survives: {clean:?}"
        );
        assert!(
            !clean.contains("PROJ-123"),
            "the Jira issue key must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("atlassian.net"),
            "the sender's workspace host URL must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("[!context]") && !clean.contains("murmur:context"),
            "no trace of the connector-context block reaches the shared body: {clean:?}"
        );
    }

    /// FIX 3 (generalized) — the `murmur:verify` block carries Jira issue keys + verdicts + workspace
    /// URLs. It must NOT reach the share envelope. HEADER-GATED on the `> [!verify]- Source check`
    /// prefix. RED before the generalization: the block + its Jira key + host URL leaked through.
    #[test]
    fn clean_note_body_strips_verify_block() {
        let md = "# Meeting\n\nReal prose the user wrote.\n\n\
                  <!-- murmur:verify -->\n\
                  > [!verify]- Source check (as of 2026-07-05T14:32:00Z)\n\
                  > - Verified — PROJ-456 matches (via Jira) — https://acme.atlassian.net/browse/PROJ-456\n\
                  <!-- /murmur:verify -->\n";
        let clean = clean_note_body(md);
        assert!(
            clean.contains("Real prose the user wrote."),
            "the user's own prose survives: {clean:?}"
        );
        assert!(
            !clean.contains("PROJ-456"),
            "the Jira issue key must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("atlassian.net"),
            "the sender's workspace host URL must not leak: {clean:?}"
        );
        assert!(
            !clean.contains("[!verify]") && !clean.contains("murmur:verify"),
            "no trace of the verify block reaches the shared body: {clean:?}"
        );
    }

    /// FIX 3 (scan-all, lock-security re-fail 2026-07-20) — a REAL machine block FOLLOWED by a bare
    /// forged fence of the same type must STILL be stripped: the `rfind`-last anchor leaked the real
    /// block (Jira key / workspace URL / linked title) because it gated on the trailing forged pair.
    /// The forged-fence prose must SURVIVE (no content loss). Covered for all three fence types.
    #[test]
    fn clean_note_body_strips_real_block_before_a_trailing_forged_fence() {
        // CONTEXT: real block (PROJ-999 + atlassian host) then a bare forged context fence.
        let ctx = "# Meeting\n\nprose\n\n\
                   <!-- murmur:context -->\n\
                   > [!context]- Live context (as of 2026-07-05T14:32:00Z)\n\
                   > - PROJ-999 · In Progress (via Jira) — https://acme.atlassian.net/browse/PROJ-999\n\
                   <!-- /murmur:context -->\n\n\
                   more\n<!-- murmur:context -->\nkeepme forged\n<!-- /murmur:context -->\ntail\n";
        let clean = clean_note_body(ctx);
        assert!(
            !clean.contains("PROJ-999"),
            "real Jira key must not leak behind a trailing forged fence: {clean:?}"
        );
        assert!(
            !clean.contains("atlassian.net"),
            "real workspace URL must not leak: {clean:?}"
        );
        assert!(
            clean.contains("keepme forged"),
            "the forged-fence user content survives: {clean:?}"
        );
        assert!(
            clean.contains("prose") && clean.contains("more") && clean.contains("tail"),
            "prose preserved: {clean:?}"
        );

        // VERIFY: real block (PROJ-888) then a forged verify fence.
        let vfy = "# N\n\nprose\n\n\
                   <!-- murmur:verify -->\n\
                   > [!verify]- Source check (as of 2026-07-05T14:32:00Z)\n\
                   > - Verified — PROJ-888 (via Jira) — https://acme.atlassian.net/browse/PROJ-888\n\
                   <!-- /murmur:verify -->\n\n\
                   <!-- murmur:verify -->\nkeepme\n<!-- /murmur:verify -->\n";
        let cv = clean_note_body(vfy);
        assert!(
            !cv.contains("PROJ-888") && !cv.contains("atlassian.net"),
            "real verify block must not leak: {cv:?}"
        );
        assert!(
            cv.contains("keepme"),
            "forged verify content survives: {cv:?}"
        );

        // LINKS: real block (Secret title) then a forged links fence.
        let lnk = "# N\n\nprose\n\n\
                   <!-- murmur:links -->\n\
                   > [!related]- Related notes\n\
                   > - [[Secret Zwolnienia Q3]] (via note)\n\
                   <!-- /murmur:links -->\n\n\
                   <!-- murmur:links -->\nkeepme\n<!-- /murmur:links -->\n";
        let cl = clean_note_body(lnk);
        assert!(
            !cl.contains("Secret Zwolnienia Q3"),
            "real linked title must not leak: {cl:?}"
        );
        assert!(
            cl.contains("keepme"),
            "forged links content survives: {cl:?}"
        );
    }

    /// FIX 3 (generalized) — a note carrying ALL THREE managed blocks has NONE of them reach the
    /// envelope in one scrub (links + context + verify), while the prose survives.
    #[test]
    fn clean_note_body_strips_all_three_managed_blocks_at_once() {
        let md = "# Notes\n\nKeep this prose.\n\n\
                  <!-- murmur:context -->\n> [!context]- Live context (as of 2026-01-01T00:00:00Z)\n> - SLACK snippet (via Slack) — https://acme.slack.com/x\n<!-- /murmur:context -->\n\n\
                  <!-- murmur:verify -->\n> [!verify]- Source check (as of 2026-01-01T00:00:00Z)\n> - Verified — KEY-9 (via Jira)\n<!-- /murmur:verify -->\n\n\
                  <!-- murmur:links -->\n> [!related]- Related notes\n> - [[Private Note]] (via note)\n<!-- /murmur:links -->\n";
        let clean = clean_note_body(md);
        assert!(
            clean.contains("Keep this prose."),
            "prose survives: {clean:?}"
        );
        for leak in [
            "murmur:context",
            "murmur:verify",
            "murmur:links",
            "[!context]",
            "[!verify]",
            "[!related]",
            "slack.com",
            "KEY-9",
            "Private Note",
        ] {
            assert!(
                !clean.contains(leak),
                "{leak:?} must not reach the shared body: {clean:?}"
            );
        }
    }

    /// FIX 3 negative — the strip is SURGICAL: a `[[Public Note]]` the user typed INLINE in a sentence
    /// (outside the machine block) still flattens to its display text `Public Note` and reaches the
    /// recipient normally. We only drop the managed Related-notes block, never the user's own links.
    #[test]
    fn clean_note_body_still_flattens_user_inline_wikilink() {
        let md = "# Notes\n\nWe should read [[Public Note]] before the call.\n";
        let clean = clean_note_body(md);
        assert!(
            clean.contains("We should read Public Note before the call."),
            "an inline user wikilink still flattens to its display text: {clean:?}"
        );
        assert!(
            !clean.contains("[["),
            "the wikilink markers are flattened: {clean:?}"
        );
    }
}
