use super::super::cursor::*;
use super::*;
use serde_json::{Value, json};
fn instance() -> Instance {
    Instance::parse(&"a".repeat(32)).unwrap()
}
fn revision() -> Revision {
    Revision::parse(&"b".repeat(64)).unwrap()
}
fn reply(era: Era) -> ReplyContext {
    ReplyContext {
        era,
        id: RpcId::String("\"".repeat(128)),
        instance: instance(),
    }
}
fn catalog(n: usize) -> ToolData {
    ToolData::RunEvidence(EvidenceData::Catalog(CatalogPage {
        kind: CatalogKind::Catalog,
        revision: revision(),
        records: (0..n)
            .map(|i| RecordSummary {
                record_id: RecordId::parse(&format!("error/{i}")).unwrap(),
                record_kind: RecordKind::Error,
                source_id: Some(ShortPreview::new(&"\u{0001}".repeat(128), false)),
                summary: ShortPreview::new(&"\u{0001}".repeat(128), false),
                outcome: None,
            })
            .collect(),
        next_cursor: None,
    }))
}
fn cursor(family: Family) -> CursorRecord {
    CursorRecord {
        family,
        instance: [0xaa; 16],
        admission: if matches!(
            family,
            Family::RecipeCatalog | Family::RecipeRecord | Family::RunList
        ) {
            0
        } else {
            1
        },
        revision: [0xbb; 32],
        selector_digest: [3; 16],
        next_position: 0,
        log_change_digest: [0; 16],
        list_high_water: 0,
        requested_limit: if matches!(
            family,
            Family::RecipeCatalog | Family::RunList | Family::EvidenceCatalog
        ) {
            32
        } else {
            4096
        },
    }
}
fn log(command: String) -> ToolData {
    ToolData::RunEvidence(EvidenceData::Log(LogPage {
        kind: LogKind::Log,
        revision: revision(),
        command_id: command,
        stream: Stream::Stdout,
        offset: UInt::new(0).unwrap(),
        bytes: vec![255; 4096],
        recorded_bytes: UInt::new(4096).unwrap(),
        current_bytes: UInt::new(4096).unwrap(),
        readable_total: UInt::new(4096).unwrap(),
        capture_truncated: false,
        change_token: "0".repeat(32),
        next_cursor: None,
    }))
}
fn parsed(frame: &OutboundFrame) -> Value {
    assert!(frame.bytes.len() <= 65536);
    assert_eq!(frame.bytes.last(), Some(&b'\n'));
    serde_json::from_slice(&frame.bytes).unwrap()
}
#[test]
fn full_escaping_keeps_identical_structured_text_and_era_fields() {
    for era in [Era::Legacy, Era::Modern] {
        let r = reply(era);
        let mut data = catalog(1);
        if let ToolData::RunEvidence(EvidenceData::Catalog(ref mut p)) = data {
            p.records[0].summary = ShortPreview::new("\"\\\n\t😀é", false);
        }
        let f = encode(era, &r.id, &r.instance, None, &Ok(data)).unwrap();
        let v = parsed(&f);
        let result = &v["result"];
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        assert_eq!(text["ok"], true);
        assert_eq!(result["isError"], false);
        assert_eq!(v["id"], json!(r.id));
        assert_eq!(
            result["resultType"].as_str(),
            if era == Era::Modern {
                Some("complete")
            } else {
                None
            }
        );
        if era == Era::Modern {
            assert_eq!(
                result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
                "theseus"
            );
        }
    }
}
#[test]
fn overflow_returns_bounded_small_failure_without_replacing_external_owner() {
    let r = reply(Era::Modern);
    let data = log("\u{0001}".repeat(4096));
    let err = encode(r.era, &r.id, &r.instance, None, &Ok(data)).unwrap_err();
    let EncodeFailure::Capacity(fallback) = err else {
        panic!("capacity fallback required")
    };
    let v = parsed(&fallback);
    assert_eq!(
        v["result"]["structuredContent"]["error"]["code"],
        "response_too_large"
    );
    assert_eq!(fallback.offset, 0);
}
#[test]
fn complete_protocol_listing_and_error_envelopes_are_bounded() {
    for era in [Era::Legacy, Era::Modern] {
        let r = reply(era);
        let v = parsed(&encode_protocol_result(era, &r.id, &Method::ToolsList).unwrap());
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 6);
    }
    for id in [
        ReplyId::Null,
        ReplyId::Omit,
        ReplyId::Id(RpcId::Integer(i64::MIN)),
    ] {
        let e = ProtocolError {
            code: -32600,
            message: TextPreview::new("bad", false),
            data: None,
            id,
            close_after_send: true,
        };
        let v = parsed(&encode_protocol_error(&e).unwrap());
        assert_eq!(v["error"]["code"], -32600);
        match e.id {
            ReplyId::Null => assert!(v["id"].is_null()),
            ReplyId::Omit => assert!(v.get("id").is_none()),
            ReplyId::Id(_) => assert_eq!(v["id"], i64::MIN),
        }
    }
}
#[test]
fn fitter_chooses_largest_prefix_and_retains_original_limit_position_and_context() {
    for era in [Era::Legacy, Era::Modern] {
        let r = reply(era);
        let mut c = cursor(Family::EvidenceCatalog);
        c.next_position = 7;
        let result = fit_page(
            &r,
            PageCandidate {
                data: catalog(32),
                cursor: c,
                more_after_candidate: false,
            },
        )
        .unwrap();
        let ToolData::RunEvidence(EvidenceData::Catalog(p)) = &result else {
            panic!()
        };
        let n = p.records.len();
        assert!((1..32).contains(&n));
        let token = CursorRecord::decode(p.next_cursor.as_ref().unwrap(), &r.instance).unwrap();
        assert_eq!(token.requested_limit, 32);
        assert_eq!(token.next_position, 7 + n as u64);
        encode(r.era, &r.id, &r.instance, None, &Ok(result)).unwrap();
        let mut larger = catalog(n + 1);
        if let ToolData::RunEvidence(EvidenceData::Catalog(ref mut p)) = larger {
            let mut c = token;
            c.next_position += 1;
            p.next_cursor = Some(c.encode().unwrap());
        }
        assert!(matches!(
            encode(r.era, &r.id, &r.instance, None, &Ok(larger)),
            Err(EncodeFailure::Capacity(_))
        ));
    }
}
#[test]
fn byte_fitter_rejects_one_byte_too_large_and_accepts_empty_final() {
    let r = reply(Era::Modern);
    let err = fit_page(
        &r,
        PageCandidate {
            data: log("\u{0001}".repeat(6000)),
            cursor: cursor(Family::Log),
            more_after_candidate: false,
        },
    )
    .unwrap_err();
    assert_eq!(err.error.code, ErrorCode::ResponseTooLarge);
    let mut empty = log("cmd".into());
    if let ToolData::RunEvidence(EvidenceData::Log(ref mut p)) = empty {
        p.bytes.clear();
        p.recorded_bytes = UInt::new(0).unwrap();
        p.current_bytes = UInt::new(0).unwrap();
        p.readable_total = UInt::new(0).unwrap();
    }
    let result = fit_page(
        &r,
        PageCandidate {
            data: empty.clone(),
            cursor: cursor(Family::Log),
            more_after_candidate: false,
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["next_cursor"],
        Value::Null
    );
    assert_eq!(
        fit_page(
            &r,
            PageCandidate {
                data: empty,
                cursor: cursor(Family::Log),
                more_after_candidate: true
            }
        )
        .unwrap_err()
        .error
        .code,
        ErrorCode::ResponseTooLarge
    );
}
#[test]
fn final_candidate_without_cursor_is_tried_before_shorter_nonfinal_pages() {
    let r = reply(Era::Legacy);
    let result = fit_page(
        &r,
        PageCandidate {
            data: catalog(1),
            cursor: cursor(Family::EvidenceCatalog),
            more_after_candidate: false,
        },
    )
    .unwrap();
    assert!(serde_json::to_value(result).unwrap()["next_cursor"].is_null());
}

#[test]
fn cap_counts_final_newline_and_rejects_before_any_growth() {
    let bytes =
        serialized(&"a".repeat(65_533), true).unwrap_or_else(|_| panic!("exact cap must fit"));
    assert_eq!(bytes.len(), 65_536);
    assert!(matches!(
        serialized(&"a".repeat(65_534), true),
        Err(StageError::Capacity)
    ));
    let mut writer = CappedWriter::new();
    writer.write_all(b"small").unwrap();
    assert!(writer.write_all(&vec![0; 65_536]).is_err());
    assert_eq!(writer.bytes, b"small");
    assert!(writer.bytes.capacity() <= 65_536);
}
#[test]
fn nonmonotonic_final_endpoint_fits_exact_cap_even_when_previous_prefix_does_not() {
    let r = reply(Era::Legacy);
    let mut data = log("\u{0001}".repeat(4096));
    if let ToolData::RunEvidence(EvidenceData::Log(ref mut p)) = data {
        p.bytes.truncate(1385);
    }
    let direct = encode(r.era, &r.id, &r.instance, None, &Ok(data.clone())).unwrap();
    assert_eq!(direct.bytes.len(), 65_536);
    let result = fit_page(
        &r,
        PageCandidate {
            data: data.clone(),
            cursor: cursor(Family::Log),
            more_after_candidate: false,
        },
    )
    .unwrap();
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["bytes"].as_array().unwrap().len(), 1385);
    assert!(value["next_cursor"].is_null());
    let next = fit_page(
        &r,
        PageCandidate {
            data,
            cursor: cursor(Family::Log),
            more_after_candidate: true,
        },
    )
    .unwrap();
    let value = serde_json::to_value(next).unwrap();
    assert_eq!(value["bytes"].as_array().unwrap().len(), 1338);
    assert!(value["next_cursor"].is_string());
}
#[test]
fn valid_maximum_escaped_command_id_shrinks_to_nonempty_byte_page() {
    for era in [Era::Legacy, Era::Modern] {
        let r = reply(era);
        let result = fit_page(
            &r,
            PageCandidate {
                data: log("\u{0001}".repeat(4096)),
                cursor: cursor(Family::Log),
                more_after_candidate: false,
            },
        )
        .unwrap();
        let value = serde_json::to_value(&result).unwrap();
        let count = value["bytes"].as_array().unwrap().len();
        assert!((1..4096).contains(&count));
        let c = CursorRecord::decode(
            &Cursor::parse(value["next_cursor"].as_str().unwrap()).unwrap(),
            &r.instance,
        )
        .unwrap();
        assert_eq!(c.next_position, count as u64);
        assert_eq!(c.requested_limit, 4096);
        encode(era, &r.id, &r.instance, None, &Ok(result)).unwrap();
    }
}
#[test]
fn one_summary_too_large_never_emits_empty_nonfinal_page() {
    let r = reply(Era::Legacy);
    let mut data = catalog(1);
    if let ToolData::RunEvidence(EvidenceData::Catalog(ref mut p)) = data {
        p.records[0].outcome = Some("\u{0001}".repeat(6000));
    }
    assert_eq!(
        fit_page(
            &r,
            PageCandidate {
                data,
                cursor: cursor(Family::EvidenceCatalog),
                more_after_candidate: false
            }
        )
        .unwrap_err()
        .error
        .code,
        ErrorCode::ResponseTooLarge
    );
}
#[test]
fn record_pages_keep_raw_byte_offsets_across_utf8_splits() {
    let r = reply(Era::Modern);
    let mut c = cursor(Family::RecipeRecord);
    c.admission = 0;
    c.next_position = 1;
    let data = ToolData::RecipeInspect(InspectData::Record(RecordPage {
        kind: RecordSelectorKind::Record,
        revision: revision(),
        record_id: RecordId::parse("env/0").unwrap(),
        record_kind: RecordKind::RecipeEnvironment,
        encoding: Encoding::JsonUtf8,
        offset: UInt::new(1).unwrap(),
        bytes: vec![0xa9, b'\\', b'n'],
        total_bytes: UInt::new(100).unwrap(),
        next_cursor: None,
    }));
    let result = fit_page(
        &r,
        PageCandidate {
            data,
            cursor: c,
            more_after_candidate: true,
        },
    )
    .unwrap();
    let v = serde_json::to_value(result).unwrap();
    assert_eq!(v["offset"], 1);
    assert_eq!(v["bytes"], json!([169, 92, 110]));
    let token = CursorRecord::decode(
        &Cursor::parse(v["next_cursor"].as_str().unwrap()).unwrap(),
        &r.instance,
    )
    .unwrap();
    assert_eq!(token.next_position, 4);
    assert_eq!(token.requested_limit, 4096);
}

#[test]
fn run_list_uses_complete_envelope_and_preserves_frozen_membership() {
    let r = reply(Era::Modern);
    let preview = TextPreview::new(&"\u{0001}".repeat(512), false);
    let runs=(1..=32).map(|i|serde_json::from_value(json!({"handle":format!("{}:{i}",r.instance.as_str()),"admission_sequence":i,"state":"running","cancellation_requested":false,"cancellation_reason":null,"report_id":preview,"report_path":preview,"journal_path":preview,"saved":null,"engine_result":null,"saved_hint_sequence":0,"snapshot_hint_sequence":null,"cleanup":{"native":"in_progress","guest":"in_progress","launch":null,"recovery_required":false},"error":null})).unwrap()).collect();
    let mut c = cursor(Family::RunList);
    c.revision = [0; 32];
    c.list_high_water = 32;
    let result = fit_page(
        &r,
        PageCandidate {
            data: ToolData::RunList(ListData {
                runs,
                high_water: 32,
                next_cursor: None,
            }),
            cursor: c,
            more_after_candidate: false,
        },
    )
    .unwrap();
    let ToolData::RunList(p) = &result else {
        panic!()
    };
    assert!((1..32).contains(&p.runs.len()));
    assert_eq!(p.high_water, 32);
    let token = CursorRecord::decode(p.next_cursor.as_ref().unwrap(), &r.instance).unwrap();
    assert_eq!(token.next_position, p.runs.len() as u64);
    assert_eq!(token.list_high_water, 32);
    assert_eq!(token.requested_limit, 32);
    let frame = encode(r.era, &r.id, &r.instance, None, &Ok(result)).unwrap();
    parsed(&frame);
}
#[test]
fn recipe_catalog_and_evidence_record_share_the_budget_owner() {
    let r = reply(Era::Legacy);
    let ToolData::RunEvidence(EvidenceData::Catalog(p)) = catalog(32) else {
        panic!()
    };
    let result = fit_page(
        &r,
        PageCandidate {
            data: ToolData::RecipeInspect(InspectData::Catalog(p)),
            cursor: cursor(Family::RecipeCatalog),
            more_after_candidate: false,
        },
    )
    .unwrap();
    let ToolData::RecipeInspect(InspectData::Catalog(p)) = &result else {
        panic!()
    };
    assert!((1..32).contains(&p.records.len()));
    assert!(p.next_cursor.is_some());
    encode(r.era, &r.id, &r.instance, None, &Ok(result)).unwrap();
    let p = RecordPage {
        kind: RecordSelectorKind::Record,
        revision: revision(),
        record_id: RecordId::parse("run").unwrap(),
        record_kind: RecordKind::RunHeader,
        encoding: Encoding::JsonUtf8,
        offset: UInt::new(0).unwrap(),
        bytes: vec![255; 4096],
        total_bytes: UInt::new(8192).unwrap(),
        next_cursor: None,
    };
    let result = fit_page(
        &r,
        PageCandidate {
            data: ToolData::RunEvidence(EvidenceData::Record(p)),
            cursor: cursor(Family::EvidenceRecord),
            more_after_candidate: true,
        },
    )
    .unwrap();
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["bytes"].as_array().unwrap().len(), 4096);
    let token = CursorRecord::decode(
        &Cursor::parse(value["next_cursor"].as_str().unwrap()).unwrap(),
        &r.instance,
    )
    .unwrap();
    assert_eq!(token.next_position, 4096);
    encode(r.era, &r.id, &r.instance, None, &Ok(result)).unwrap();
}
#[test]
fn recipe_cursor_must_match_reply_instance_before_encoding_a_page() {
    let r = reply(Era::Modern);
    let mut c = cursor(Family::RecipeCatalog);
    c.instance = [0xcc; 16];
    let ToolData::RunEvidence(EvidenceData::Catalog(p)) = catalog(1) else {
        panic!()
    };
    assert_eq!(
        fit_page(
            &r,
            PageCandidate {
                data: ToolData::RecipeInspect(InspectData::Catalog(p)),
                cursor: c,
                more_after_candidate: false
            }
        )
        .unwrap_err()
        .error
        .code,
        ErrorCode::StaleInstance
    );
}

#[test]
fn capacity_fallback_preserves_trusted_admission_handle() {
    let r = reply(Era::Modern);
    let handle = Handle::parse(&format!("{}:1", r.instance.as_str())).unwrap();
    let data = log("\u{0001}".repeat(4096));
    let EncodeFailure::Capacity(frame) =
        encode(r.era, &r.id, &r.instance, Some(&handle), &Ok(data)).unwrap_err()
    else {
        panic!()
    };
    assert_eq!(
        parsed(&frame)["result"]["structuredContent"]["error"]["handle"],
        handle.as_str()
    );
    let failure = fit_page(
        &r,
        PageCandidate {
            data: log("\u{0001}".repeat(6000)),
            cursor: cursor(Family::Log),
            more_after_candidate: false,
        },
    )
    .unwrap_err();
    assert_eq!(failure.error.handle.as_ref(), Some(&handle));
}

#[test]
fn fallback_capacity_failure_is_fatal_and_protocol_version_data_stays_bounded() {
    let r = reply(Era::Modern);
    let invalid_internal_id = RpcId::String("x".repeat(65_536));
    assert!(matches!(
        encode(
            r.era,
            &invalid_internal_id,
            &r.instance,
            None,
            &Ok(catalog(1))
        ),
        Err(EncodeFailure::Fatal)
    ));
    let mut error = ProtocolError {
        code: -32022,
        message: TextPreview::new("Unsupported protocol version", false),
        data: Some(VersionData {
            requested: "next".into(),
            supported: vec![MODERN_VERSION.into()],
        }),
        id: ReplyId::Id(RpcId::Integer(0)),
        close_after_send: false,
    };
    let value = parsed(&encode_protocol_error(&error).unwrap());
    assert_eq!(
        value["error"]["data"],
        json!({"requested":"next","supported":["2026-07-28"]})
    );
    error.data.as_mut().unwrap().requested = "x".repeat(65_536);
    assert!(matches!(
        encode_protocol_error(&error),
        Err(EncodeFailure::Fatal)
    ));
}
