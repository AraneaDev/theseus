use super::*;
fn instance() -> Instance {
    Instance::parse(&"ab".repeat(16)).unwrap()
}
fn recipe(record: bool) -> InspectArgs {
    serde_json::from_value(serde_json::json!({"recipe_path":"r.toml","recipe_digest":"12".repeat(32),"selector":if record { serde_json::json!({"kind":"record","record_id":"env/0"}) } else { serde_json::json!({"kind":"catalog","collection":"steps"}) }})).unwrap()
}
fn evidence(kind: &str) -> EvidenceArgs {
    serde_json::from_value(serde_json::json!({"server_instance":instance(),"handle":format!("{}:128",instance().as_str()),"revision":"34".repeat(32),"selector":match kind {"catalog"=>serde_json::json!({"kind":"catalog","collection":"run"}),"record"=>serde_json::json!({"kind":"record","record_id":"run"}),_=>serde_json::json!({"kind":"log","command_id":"cmd","stream":"stdout"})}})).unwrap()
}
fn families() -> Vec<CursorRecord> {
    vec![
        CursorRecord::recipe(&instance(), &recipe(false)).unwrap(),
        CursorRecord::recipe(&instance(), &recipe(true)).unwrap(),
        CursorRecord::list(
            &ListArgs {
                server_instance: instance(),
                limit: None,
                cursor: None,
            },
            128,
        )
        .unwrap(),
        CursorRecord::evidence(
            &evidence("catalog"),
            &Revision::parse(&"34".repeat(32)).unwrap(),
        )
        .unwrap(),
        CursorRecord::evidence(
            &evidence("record"),
            &Revision::parse(&"34".repeat(32)).unwrap(),
        )
        .unwrap(),
        CursorRecord::evidence(
            &evidence("log"),
            &Revision::parse(&"34".repeat(32)).unwrap(),
        )
        .unwrap()
        .with_log_fingerprint(b"native fixed bytes")
        .unwrap(),
    ]
}
#[test]
fn six_families_roundtrip_fixed_big_endian_fields() {
    for (i, mut c) in families().into_iter().enumerate() {
        c.next_position = if c.family == Family::RunList {
            127
        } else {
            0x01020304050607
        };
        let token = c.encode().unwrap();
        let s = token.as_str();
        assert_eq!(s.len(), 188);
        assert_eq!(&s[..4], format!("01{:02x}", i + 1));
        assert_eq!(&s[4..36], "ab".repeat(16));
        assert_eq!(
            &s[134..150],
            if c.family == Family::RunList {
                "000000000000007f"
            } else {
                "0001020304050607"
            }
        );
        assert_eq!(CursorRecord::decode(&token, &instance()).unwrap(), c);
    }
}
#[test]
fn malformed_unused_and_cross_instance_tokens_are_rejected() {
    let c = families()[2].clone();
    let token = c.encode().unwrap();
    for (offset, value) in [(0, "02"), (2, "07"), (36, "01"), (38, "01"), (150, "01")] {
        let mut s = token.as_str().to_owned();
        s.replace_range(offset..offset + 2, value);
        assert_eq!(
            CursorRecord::decode(&Cursor::parse(&s).unwrap(), &instance()).unwrap_err(),
            ErrorCode::InvalidCursor
        );
    }
    for s in ["A".repeat(188), "0".repeat(186), "g".repeat(188)] {
        assert_eq!(
            CursorRecord::decode(&Cursor::parse(&s).unwrap(), &instance()).unwrap_err(),
            ErrorCode::InvalidCursor
        );
    }
    assert_eq!(
        CursorRecord::decode(&token, &Instance::parse(&"cd".repeat(16)).unwrap()).unwrap_err(),
        ErrorCode::StaleInstance
    );
    let mut bad = families()[0].clone();
    bad.list_high_water = 1;
    assert_eq!(bad.encode().unwrap_err(), ErrorCode::InvalidCursor);
    bad.list_high_water = 0;
    bad.log_change_digest[0] = 1;
    assert_eq!(bad.encode().unwrap_err(), ErrorCode::InvalidCursor);
}
#[test]
fn selector_limit_revision_log_and_endpoint_are_checked_before_access() {
    for c in families() {
        let total = if c.family == Family::RunList { 128 } else { 1 };
        assert!(c.validate(&c, total).is_ok());
        assert_eq!(c.validate(&c, 0), Err(ErrorCode::InvalidCursor));
        let mut past = c.clone();
        past.next_position = 1;
        assert_eq!(past.validate(&c, 1), Err(ErrorCode::InvalidCursor));
        let mut bad = c.clone();
        bad.selector_digest[0] ^= 1;
        assert_eq!(bad.validate(&c, 1), Err(ErrorCode::InvalidCursor));
        let mut bad = c.clone();
        bad.requested_limit = 1;
        assert_eq!(bad.validate(&c, 1), Err(ErrorCode::InvalidCursor));
        if c.family != Family::RunList {
            let mut bad = c.clone();
            bad.revision[0] ^= 1;
            assert_eq!(
                bad.validate(&c, 1),
                Err(
                    if matches!(c.family, Family::RecipeCatalog | Family::RecipeRecord) {
                        ErrorCode::RecipeChanged
                    } else {
                        ErrorCode::RevisionChanged
                    }
                )
            );
        }
    }
    let c = families()[5].clone();
    let changed = c.clone().with_log_fingerprint(b"replacement").unwrap();
    assert_eq!(c.validate(&changed, 1), Err(ErrorCode::LogChanged));
    assert_eq!(c.change_token().len(), 32);
    let mut c = families()[2].clone();
    c.list_high_water = 127;
    assert_eq!(
        c.validate(&families()[2], 128),
        Err(ErrorCode::InvalidCursor)
    );
}
#[test]
fn typed_selection_normalizes_defaults_excludes_limit_and_preserves_selection_order() {
    let a = recipe(false);
    let mut b = a.clone();
    b.limit = Some(UInt::new(32).unwrap());
    b.cursor = Some(Cursor::parse("ignored").unwrap());
    assert_eq!(
        CursorRecord::recipe(&instance(), &a).unwrap(),
        CursorRecord::recipe(&instance(), &b).unwrap()
    );
    b.selector = InspectSelector::Catalog {
        collection: InspectCollection::Checks,
        profile: None,
    };
    assert_ne!(
        CursorRecord::recipe(&instance(), &a)
            .unwrap()
            .selector_digest,
        CursorRecord::recipe(&instance(), &b)
            .unwrap()
            .selector_digest
    );
    let mut a = evidence("log");
    let old = CursorRecord::evidence(&a, a.revision.as_ref().unwrap()).unwrap();
    if let EvidenceSelector::Log(ref mut s) = a.selector {
        s.stream = Stream::Stderr;
    }
    assert_ne!(
        old.selector_digest,
        CursorRecord::evidence(&a, a.revision.as_ref().unwrap())
            .unwrap()
            .selector_digest
    );
}

#[test]
fn cursor_integer_limits_unused_fields_and_known_typed_hash_are_exact() {
    let recipe = CursorRecord::recipe(&instance(), &recipe(false)).unwrap();
    assert_eq!(
        to_hex(&recipe.selector_digest),
        "81a96085d806d80b3f9469bd8925c703"
    );
    let list = CursorRecord::list(
        &ListArgs {
            server_instance: instance(),
            limit: None,
            cursor: None,
        },
        128,
    )
    .unwrap();
    assert_eq!(
        to_hex(&list.selector_digest),
        "d0973becfb6cb7b82d84d6bb3106b504"
    );
    for mut c in families() {
        c.requested_limit = 0;
        assert_eq!(c.encode().unwrap_err(), ErrorCode::InvalidCursor);
        c.requested_limit = c.family.maximum() + 1;
        assert_eq!(c.encode().unwrap_err(), ErrorCode::InvalidCursor);
    }
    let mut c = recipe;
    c.next_position = UINT_MAX;
    assert!(c.encode().is_ok());
    assert_eq!(c.validate(&c, UINT_MAX), Err(ErrorCode::InvalidCursor));
    c.next_position = UINT_MAX + 1;
    assert_eq!(c.encode().unwrap_err(), ErrorCode::InvalidCursor);
}
