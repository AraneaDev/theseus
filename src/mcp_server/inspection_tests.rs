use super::*;
#[test]
fn overview_reads_valid_recipe_without_environment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(
        root.join("recipe.toml"),
        include_str!("../../tests/fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    let roots = Roots::open(&root, &root.join("output")).unwrap();
    let args = InspectArgs {
        recipe_path: "recipe.toml".into(),
        recipe_digest: None,
        selector: InspectSelector::Overview,
        limit: None,
        cursor: None,
    };
    let reply = ReplyContext {
        era: Era::Modern,
        id: RpcId::Integer(1),
        instance: Instance::parse(&"a".repeat(32)).unwrap(),
    };
    let InspectData::Overview(data) = inspect(&roots, &args, 3600, &reply).unwrap() else {
        panic!()
    };
    assert_eq!(data.schema_version, 2);
    assert_eq!(data.max_run_seconds.get(), 3600);
}

#[test]
fn catalogs_details_redact_only_typed_env_and_preserve_scripts_and_proof_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    let source = include_str!("../../tests/fixtures/recipes/v2_clean.toml")
        .replace("schema_version = 2", "schema_version = 7")
        .replace("script = \"true\"", "script = \"literal-script-é\"\nenv = { TOKEN = \"literal-secret-value\" }\npass_env = [\"FORWARDED\"]")
        + "\n[[profiles.user.checks]]\nid='source-id-complete'\ncheckpoint='configured'\nkind='json_command'\nscript='check-script'\nenv={CHECK_TOKEN='literal-secret-value'}\npass_env=['CHECK_FORWARDED']\nexpected_exit=0\nassertions=[{kind='equals',pointer='/env',equals_json='\"semantic-env-value\"'}]\n";
    std::fs::write(root.join("recipe.toml"), source).unwrap();
    let roots = Roots::open(&root, &root.join("output")).unwrap();
    let reply = ReplyContext {
        era: Era::Modern,
        id: RpcId::Integer(1),
        instance: Instance::parse(&"a".repeat(32)).unwrap(),
    };
    let digest = crate::mcp_server::roots::read_recipe(&roots, "recipe.toml")
        .unwrap()
        .digest;
    for collection in [
        InspectCollection::Steps,
        InspectCollection::Checks,
        InspectCollection::Artifacts,
        InspectCollection::Environment,
        InspectCollection::ObservedRoots,
        InspectCollection::Policies,
    ] {
        let args = InspectArgs {
            recipe_path: "recipe.toml".into(),
            recipe_digest: Some(Digest::parse(&digest).unwrap()),
            selector: InspectSelector::Catalog {
                collection,
                profile: None,
            },
            limit: None,
            cursor: None,
        };
        let InspectData::Catalog(page) = inspect(&roots, &args, 3600, &reply).unwrap() else {
            panic!()
        };
        for summary in page.records {
            let mut args = args.clone();
            args.selector = InspectSelector::Record {
                record_id: summary.record_id,
            };
            args.limit = Some(UInt::new(7).unwrap());
            let mut bytes = vec![];
            loop {
                let InspectData::Record(page) = inspect(&roots, &args, 3600, &reply).unwrap()
                else {
                    panic!()
                };
                assert_eq!(page.offset.get(), bytes.len() as u64);
                bytes.extend(page.bytes);
                args.cursor = page.next_cursor;
                if args.cursor.is_none() {
                    assert_eq!(page.total_bytes.get(), bytes.len() as u64);
                    break;
                }
            }
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("literal-secret-value"));
            if json.get("check").is_some() {
                assert!(text.contains("source-id-complete"));
                assert!(text.contains("semantic-env-value"));
                assert_eq!(
                    json["check"]["declared_env_names"],
                    serde_json::json!(["CHECK_TOKEN"])
                );
            }
        }
    }
}

#[path = "inspection_fixtures.rs"]
mod mcp_fixture;
#[path = "../../tests/support/plugin.rs"]
mod plugin_fixture;

fn setup(source: &str) -> (tempfile::TempDir, Roots, InspectArgs, ReplyContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("target/client")).unwrap();
    std::fs::create_dir(root.join("base")).unwrap();
    std::fs::write(root.join("target/client/claude"), "synthetic client").unwrap();
    std::fs::write(root.join("recipe.toml"), source).unwrap();
    let roots = Roots::open(&root, &root.join("output")).unwrap();
    let args = InspectArgs {
        recipe_path: "recipe.toml".into(),
        recipe_digest: None,
        selector: InspectSelector::Overview,
        limit: None,
        cursor: None,
    };
    let reply = ReplyContext {
        era: Era::Modern,
        id: RpcId::Integer(1),
        instance: Instance::parse(&"a".repeat(32)).unwrap(),
    };
    (temp, roots, args, reply)
}
#[test]
fn all_eight_schema_fixtures_use_real_parser_and_preserve_declared_metadata() {
    let mut sources = vec![
        include_str!("../../tests/fixtures/recipes/valid.toml").to_owned(),
        include_str!("../../tests/fixtures/recipes/v2_clean.toml").to_owned(),
        include_str!("../../tests/fixtures/recipes/v3_interruption.toml").to_owned(),
        include_str!("../../tests/fixtures/recipes/v4_downgrade.toml").to_owned(),
        include_str!("../../tests/fixtures/recipes/rollback.toml").to_owned(),
        include_str!("../../tests/fixtures/recipes/interrupted_rollback.toml").to_owned(),
    ];
    sources.push(toml::to_string(&plugin_fixture::declaration("user")).unwrap());
    sources.push(toml::to_string(&mcp_fixture::declaration()).unwrap());
    for (index, source) in sources.iter().enumerate() {
        let (_temp, roots, args, reply) = setup(source);
        let original =
            crate::recipe::parse_bytes(&roots.project_path.join("recipe.toml"), source.as_bytes())
                .unwrap();
        let InspectData::Overview(data) = inspect(&roots, &args, 15, &reply).unwrap() else {
            panic!()
        };
        assert_eq!(data.schema_version, (index + 1) as u8);
        assert_eq!(
            data.profiles,
            original.recipe.profiles.keys().copied().collect::<Vec<_>>()
        );
        assert_eq!(data.scenarios, original.declared_scenarios());
        assert_eq!(data.recipe_digest.as_str(), original.digest);
    }
}
#[test]
fn environment_profile_filter_preserves_global_ids_and_owner_order() {
    let mut value: toml::Value =
        toml::from_str(include_str!("../../tests/fixtures/recipes/valid.toml")).unwrap();
    for profile in ["user", "system"] {
        for (_name, step) in value["profiles"][profile]["steps"].as_table_mut().unwrap() {
            step.as_table_mut().unwrap().insert(
                "env".into(),
                toml::Value::try_from(std::collections::BTreeMap::from([
                    ("Z", "secret"),
                    ("A", "secret"),
                ]))
                .unwrap(),
            );
            step.as_table_mut().unwrap().insert(
                "pass_env".into(),
                toml::Value::Array(vec!["Y".into(), "B".into()]),
            );
        }
    }
    let (_temp, roots, mut args, reply) = setup(&toml::to_string(&value).unwrap());
    args.recipe_digest = Some(
        Digest::parse(
            &crate::mcp_server::roots::read_recipe(&roots, "recipe.toml")
                .unwrap()
                .digest,
        )
        .unwrap(),
    );
    let mut ids = vec![];
    for profile in [None, Some(crate::model::ProfileName::System)] {
        args.selector = InspectSelector::Catalog {
            collection: InspectCollection::Environment,
            profile,
        };
        args.cursor = None;
        let mut current = vec![];
        loop {
            let InspectData::Catalog(page) = inspect(&roots, &args, 1, &reply).unwrap() else {
                panic!()
            };
            current.extend(page.records.iter().map(|r| r.record_id.as_str().to_owned()));
            args.cursor = page.next_cursor;
            if args.cursor.is_none() {
                break;
            }
        }
        if profile.is_none() {
            ids = current;
        } else {
            assert!(!current.is_empty());
            assert_ne!(current[0], "env/0");
            assert!(ids.ends_with(&current));
        }
    }
}
#[test]
fn continuation_binds_limit_selector_digest_instance_and_range() {
    let (_temp, roots, mut args, reply) =
        setup(include_str!("../../tests/fixtures/recipes/v2_clean.toml"));
    args.recipe_digest = Some(
        Digest::parse(
            &crate::mcp_server::roots::read_recipe(&roots, "recipe.toml")
                .unwrap()
                .digest,
        )
        .unwrap(),
    );
    args.selector = InspectSelector::Catalog {
        collection: InspectCollection::Steps,
        profile: None,
    };
    args.limit = Some(UInt::new(1).unwrap());
    let InspectData::Catalog(page) = inspect(&roots, &args, 1, &reply).unwrap() else {
        panic!()
    };
    assert_eq!(
        page.records[0].record_id.as_str(),
        "p/user/step/install_target"
    );
    args.cursor = page.next_cursor;
    let mut wrong = args.clone();
    wrong.limit = Some(UInt::new(2).unwrap());
    assert_eq!(
        inspect(&roots, &wrong, 1, &reply).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    wrong = args.clone();
    wrong.selector = InspectSelector::Catalog {
        collection: InspectCollection::Checks,
        profile: None,
    };
    assert_eq!(
        inspect(&roots, &wrong, 1, &reply).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    let mut other = reply.clone();
    other.instance = Instance::parse(&"b".repeat(32)).unwrap();
    assert_eq!(
        inspect(&roots, &args, 1, &other).unwrap_err().code,
        ErrorCode::StaleInstance
    );
    let mut token = CursorRecord::decode(args.cursor.as_ref().unwrap(), &reply.instance).unwrap();
    token.next_position = 4;
    wrong = args.clone();
    wrong.cursor = Some(token.encode().unwrap());
    assert_eq!(
        inspect(&roots, &wrong, 1, &reply).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    std::fs::write(
        roots.project_path.join("recipe.toml"),
        include_str!("../../tests/fixtures/recipes/v2_clean.toml").replace("clean-only", "edited"),
    )
    .unwrap();
    assert_eq!(
        inspect(&roots, &args, 1, &reply).unwrap_err().code,
        ErrorCode::RecipeChanged
    );
}

#[test]
fn catalog_uses_largest_complete_envelope_prefix() {
    let mut value = serde_json::to_value(
        toml::from_str::<toml::Value>(include_str!("../../tests/fixtures/recipes/v2_clean.toml"))
            .unwrap(),
    )
    .unwrap();
    value["profiles"]["user"]["checks"]=(0..32).map(|i|serde_json::json!({"id":format!("{i}{}","\u{1}".repeat(127)),"checkpoint":"configured","kind":"command","script":"exit 99","expected_exit":0})).collect::<Vec<_>>().into();
    let (_temp, roots, mut args, mut reply) = setup(&toml::to_string(&value).unwrap());
    args.recipe_digest = Some(
        Digest::parse(
            &crate::mcp_server::roots::read_recipe(&roots, "recipe.toml")
                .unwrap()
                .digest,
        )
        .unwrap(),
    );
    args.selector = InspectSelector::Catalog {
        collection: InspectCollection::Checks,
        profile: None,
    };
    reply.id = RpcId::String("\u{1}".repeat(128));
    let InspectData::Catalog(page) = inspect(&roots, &args, 1, &reply).unwrap() else {
        panic!()
    };
    let count = page.records.len();
    assert!(count > 0 && count < 32);
    let encoded = super::super::budget::encode(
        reply.era,
        &reply.id,
        &reply.instance,
        None,
        &Ok(ToolData::RecipeInspect(InspectData::Catalog(page.clone()))),
    )
    .unwrap();
    assert!(encoded.bytes.len() <= 65536);
    let mut cursor =
        CursorRecord::decode(page.next_cursor.as_ref().unwrap(), &reply.instance).unwrap();
    assert_eq!(cursor.next_position, count as u64);
    assert_eq!(cursor.requested_limit, 32);
    let validated = crate::mcp_server::roots::read_recipe(&roots, "recipe.toml").unwrap();
    let records = inspection_records::records(&validated);
    let mut oversized = page;
    oversized.records = records
        .iter()
        .filter(|r| r.collection() == InspectCollection::Checks)
        .take(count + 1)
        .map(|r| r.summary().unwrap())
        .collect();
    cursor.next_position = (count + 1) as u64;
    oversized.next_cursor = Some(cursor.encode().unwrap());
    assert!(matches!(
        super::super::budget::encode(
            reply.era,
            &reply.id,
            &reply.instance,
            None,
            &Ok(ToolData::RecipeInspect(InspectData::Catalog(oversized)))
        ),
        Err(super::super::budget::EncodeFailure::Capacity(_))
    ));
}

#[test]
fn all_collections_and_nested_probe_details_preserve_complete_model_semantics() {
    let mut mcp = mcp_fixture::declaration();
    for check in mcp["profiles"]["user"]["checks"].as_array_mut().unwrap() {
        if check["kind"] == "mcp_stdio" {
            check["probe"]["env"] = serde_json::json!({"PROBE_TOKEN":"literal-secret-value"});
        }
        if check["kind"] == "json_command" {
            check["env"] = serde_json::json!({"JSON_TOKEN":"literal-secret-value"});
        }
    }
    let mut values = vec![mcp, plugin_fixture::declaration("user")];
    for source in [
        include_str!("../../tests/fixtures/recipes/v4_downgrade.toml"),
        include_str!("../../tests/fixtures/recipes/rollback.toml"),
        include_str!("../../tests/fixtures/recipes/interrupted_rollback.toml"),
    ] {
        values.push(serde_json::to_value(toml::from_str::<toml::Value>(source).unwrap()).unwrap());
    }
    for mut value in values {
        for profile in value["profiles"].as_object_mut().unwrap().values_mut() {
            if let Some(checks) = profile
                .get_mut("checks")
                .and_then(serde_json::Value::as_array_mut)
            {
                for check in checks {
                    if matches!(
                        check["kind"].as_str(),
                        Some("command" | "json_command" | "executable")
                    ) {
                        check["env"] = serde_json::json!({"TYPED_TOKEN":"literal-secret-value"});
                    }
                }
            }
        }
        let (_temp, roots, mut args, reply) = setup(&toml::to_string(&value).unwrap());
        let validated = crate::mcp_server::roots::read_recipe(&roots, "recipe.toml").unwrap();
        let model = serde_json::to_value(&validated.recipe).unwrap();
        args.recipe_digest = Some(Digest::parse(&validated.digest).unwrap());
        for collection in [
            InspectCollection::Steps,
            InspectCollection::Checks,
            InspectCollection::Artifacts,
            InspectCollection::Environment,
            InspectCollection::ObservedRoots,
            InspectCollection::Policies,
        ] {
            args.selector = InspectSelector::Catalog {
                collection,
                profile: None,
            };
            args.cursor = None;
            args.limit = None;
            let InspectData::Catalog(page) = inspect(&roots, &args, 1, &reply).unwrap() else {
                panic!()
            };
            for summary in page.records {
                let mut detail = args.clone();
                detail.selector = InspectSelector::Record {
                    record_id: summary.record_id.clone(),
                };
                let mut bytes = vec![];
                loop {
                    let InspectData::Record(p) = inspect(&roots, &detail, 1, &reply).unwrap()
                    else {
                        panic!()
                    };
                    bytes.extend(p.bytes);
                    detail.cursor = p.next_cursor;
                    if detail.cursor.is_none() {
                        break;
                    }
                }
                let text = String::from_utf8(bytes).unwrap();
                assert!(!text.contains("literal-secret-value"));
                let actual: serde_json::Value = serde_json::from_str(&text).unwrap();
                if summary.record_kind == RecordKind::RecipeCheck {
                    let profile = actual["profile"].as_str().unwrap();
                    let index = actual["index"].as_u64().unwrap() as usize;
                    let mut expected = model["profiles"][profile]["checks"][index].clone();
                    let expected_env = if expected["kind"] == "mcp_stdio" {
                        &mut expected["probe"]
                    } else {
                        &mut expected
                    };
                    if let Some(env) = expected_env.as_object_mut().unwrap().remove("env") {
                        expected_env["declared_env_names"] = env
                            .as_object()
                            .unwrap()
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .into();
                    }
                    assert_eq!(actual["check"], expected);
                }
                if summary.record_kind == RecordKind::RecipePolicy {
                    let profile = actual["profile"].as_str().unwrap();
                    let kind = actual["kind"].as_str().unwrap();
                    assert_eq!(actual["policy"], model["profiles"][profile][kind]);
                }
            }
        }
    }
}

#[test]
fn record_pages_split_raw_utf8_and_reassemble_full_large_script() {
    let mut value = serde_json::to_value(
        toml::from_str::<toml::Value>(include_str!("../../tests/fixtures/recipes/v2_clean.toml"))
            .unwrap(),
    )
    .unwrap();
    let script = "é\n\"\\".repeat(1200);
    value["profiles"]["user"]["steps"]["configure"]["script"] = script.clone().into();
    let (_temp, roots, mut args, reply) = setup(&toml::to_string(&value).unwrap());
    args.recipe_digest = Some(
        Digest::parse(
            &crate::mcp_server::roots::read_recipe(&roots, "recipe.toml")
                .unwrap()
                .digest,
        )
        .unwrap(),
    );
    args.selector = InspectSelector::Record {
        record_id: RecordId::parse("p/user/step/configure").unwrap(),
    };
    args.limit = Some(UInt::new(257).unwrap());
    let mut bytes = vec![];
    let mut split = false;
    loop {
        let InspectData::Record(page) = inspect(&roots, &args, 1, &reply).unwrap() else {
            panic!()
        };
        assert_eq!(page.offset.get(), bytes.len() as u64);
        split |= std::str::from_utf8(&page.bytes).is_err();
        bytes.extend(page.bytes);
        args.cursor = page.next_cursor;
        if args.cursor.is_none() {
            assert_eq!(page.total_bytes.get(), bytes.len() as u64);
            break;
        }
    }
    assert!(split);
    let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(actual["script"], script);
}
