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

/// The full "clean text" transform: strip frontmatter, then flatten wikilinks + strip
/// `obsidian://` refs. This is the single call `share_note_to_link` makes on a note's markdown
/// before building the [`murmur_protocol::envelope::ShareEnvelope`]. Pure.
pub fn clean_note_body(markdown: &str) -> String {
    let body = strip_frontmatter(markdown);
    flatten_wikilinks(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
