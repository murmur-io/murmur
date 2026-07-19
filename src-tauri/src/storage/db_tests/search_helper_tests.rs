    use super::{escape_like, excerpt, fts_match_query};

    #[test]
    fn escape_like_escapes_wildcards() {
        // '%' → '\%', '_' → '\_', '\' → '\\'
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }

    #[test]
    fn fts_match_query_quotes_terms_and_drops_operators() {
        // Each alnum token becomes a quoted literal joined by implicit-AND whitespace.
        assert_eq!(
            fts_match_query("alpha beta"),
            Some("\"alpha\" \"beta\"".into())
        );
        // Order is just term order; the conjunction is order-independent at the SQL level.
        assert_eq!(
            fts_match_query("beta alpha"),
            Some("\"beta\" \"alpha\"".into())
        );
        // FTS5 operators / punctuation are stripped, leaving only the literal terms.
        assert_eq!(
            fts_match_query("a* b\"c( AND d:e"),
            Some("\"a\" \"b\" \"c\" \"AND\" \"d\" \"e\"".into())
        );
        // Unicode (Polish) is alphanumeric → preserved as a quoted term.
        assert_eq!(fts_match_query("budżet!"), Some("\"budżet\"".into()));
        // Empty / punctuation-only → None (caller returns no hits, never errors MATCH).
        assert_eq!(fts_match_query(""), None);
        assert_eq!(fts_match_query("   "), None);
        assert_eq!(fts_match_query("\"*():^"), None);
    }

    #[test]
    fn excerpt_centers_on_match_with_ellipses() {
        // Match sits deep in a long string → window is clipped on both sides.
        let text = format!("{}needle{}", "alpha ".repeat(20), " omega".repeat(20));
        let e = excerpt(&text, "needle");
        assert!(e.contains("needle"));
        assert!(e.starts_with('…'), "leading ellipsis, got: {e}");
        assert!(e.ends_with('…'), "trailing ellipsis, got: {e}");
    }

    #[test]
    fn excerpt_handles_unicode_safely() {
        let text = "zażółć gęślą jaźń ąśćńółżź omówiliśmy budżet i planowanie na przyszły kwartał szczegółowo";
        let e = excerpt(text, "budżet");
        assert!(e.contains("budżet"));
    }
