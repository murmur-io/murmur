    use super::*;

    #[test]
    fn free_up_space_is_noop_without_a_cap() {
        let p = crate::storage::db::unique_temp_path("murmur-cmd-storage", "sqlite");
        let _ = std::fs::remove_file(&p);
        let state = AppState::init_at(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        // No limit set (default None) → free_up_space must be an inert zero summary.
        let s = crate::storage::usage::prune_to_limit(
            &state.db,
            &crate::pipeline::audio_dir().unwrap(),
            u64::MAX,
            None,
        )
        .unwrap();
        assert_eq!(s.freed_bytes, 0);
        let _ = std::fs::remove_file(&p);
    }
