use serde_json::json;
use theseus_native_protocol::v2::{self, broker::*};
#[test]
fn original_entry_preparation_has_durable_acceptance_and_exact_run_digest() {
    let run = v2::Request::Run {
        lease: v2::LeaseId::from_bytes([1; 16]),
        operation: v2::OperationId::from_bytes([2; 16]),
        program: v2::ProgramId::new("configure").unwrap(),
    };
    let preparation = json!({"helper_lease":"01".repeat(16),"broker_lease":"03".repeat(16),"operation":"02".repeat(16),"request_digest":v2::request_digest(&run).unwrap().to_hex(),"program":"configure","slot":"desktop","header_digest":"04".repeat(32),"binding_digest":"05".repeat(32),"generation":null,"prior_sut":null});
    let event = serde_json::from_value::<BrokerEvent>(
        json!({"kind":"entry_preparation_accepted","preparation":preparation}),
    );
    assert!(
        event.is_ok(),
        "original pre-SUT preparation must have its own durable receipt"
    );
}

fn fixture() -> (EntryPreparation, Vec<v2::ledger::Record>) {
    use v2::ledger::*;
    let header:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"explicit-source","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
    let run = v2::Request::Run {
        lease: v2::LeaseId::from_bytes([1; 16]),
        operation: v2::OperationId::from_bytes([2; 16]),
        program: v2::ProgramId::new("install_target").unwrap(),
    };
    let preparation = EntryPreparation {
        helper_lease: "01".repeat(16),
        broker_lease: "03".repeat(16),
        operation: "02".repeat(16),
        request_digest: v2::request_digest(&run).unwrap().to_hex(),
        program: v2::ProgramId::new("install_target").unwrap(),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        header_digest: header.digest().unwrap().to_hex(),
        binding_digest: "05".repeat(32),
        generation: None,
        prior_sut: None,
    };
    let records = vec![
        Record::new(0, Entry::IntegrationBrokerHeader { header }).unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: v2::IntentId::from_bytes([7; 16]),
                lease: v2::LeaseId::from_bytes([3; 16]),
            },
        )
        .unwrap(),
    ];
    (preparation, records)
}
fn push(records: &mut Vec<v2::ledger::Record>, event: BrokerEvent) {
    records.push(
        v2::ledger::Record::new(records.len() as u16, v2::ledger::Entry::Broker { event }).unwrap(),
    );
}
#[test]
fn entry_preparation_pending_and_quarantine_block_release_and_duplicate_authority() {
    use v2::ledger::*;
    let (p, mut records) = fixture();
    push(
        &mut records,
        BrokerEvent::EntryPreparationAccepted {
            preparation: p.clone(),
        },
    );
    assert!(BrokerLedgerView::replay(&records).unwrap().entry_blocked());
    let mut closed = records.clone();
    closed.push(
        Record::new(
            closed.len() as u16,
            Entry::LeaseClosed {
                lease: v2::LeaseId::from_bytes([3; 16]),
                disposition: Disposition::AwaitingExternalReset,
            },
        )
        .unwrap(),
    );
    assert!(BrokerLedgerView::replay(&closed).is_err());
    let mut duplicate = records.clone();
    push(
        &mut duplicate,
        BrokerEvent::EntryPreparationAccepted {
            preparation: p.clone(),
        },
    );
    assert!(BrokerLedgerView::replay(&duplicate).is_err());
    let mut quarantined = records.clone();
    push(&mut quarantined, BrokerEvent::EntryProofQuarantined {});
    push(
        &mut quarantined,
        BrokerEvent::EntryPreparationSettled {
            operation: p.operation.clone(),
        },
    );
    assert!(BrokerLedgerView::replay(&quarantined).is_err());
    push(
        &mut records,
        BrokerEvent::EntryPreparationSettled {
            operation: p.operation.clone(),
        },
    );
    let view = BrokerLedgerView::replay(&records).unwrap();
    assert!(!view.entry_blocked());
    assert_eq!(view.entry_preparations(), &[(p.clone(), true)]);
    push(
        &mut records,
        BrokerEvent::EntryPreparationSettled {
            operation: p.operation,
        },
    );
    assert!(BrokerLedgerView::replay(&records).is_err());
}
#[test]
fn entry_preparation_rejects_changed_run_digest_and_readers() {
    let (p, records) = fixture();
    for field in ["digest", "program", "lease", "operation"] {
        let mut bad = p.clone();
        match field {
            "digest" => bad.request_digest = "ff".repeat(32),
            "program" => bad.program = v2::ProgramId::new("check_target").unwrap(),
            "lease" => bad.helper_lease = "08".repeat(16),
            _ => bad.operation = "09".repeat(16),
        };
        assert!(bad.validate().is_err());
        let mut changed = records.clone();
        push(
            &mut changed,
            BrokerEvent::EntryPreparationAccepted { preparation: bad },
        );
        assert!(BrokerLedgerView::replay(&changed).is_err());
    }
}
fn guard(broker: bool) -> (v2::SessionGuard, v2::SessionId) {
    use v2::*;
    let (_, records) = fixture();
    let v2::ledger::Entry::IntegrationBrokerHeader { header } = records[0].entry() else {
        unreachable!()
    };
    let sid = SessionId::from_bytes([9; 16]);
    let mut guard = SessionGuard::new(sid.clone(), Role::Controller);
    guard
        .accept_outbound(
            &Envelope::new(
                sid.clone(),
                0,
                Message::Hello {
                    role: Role::Controller,
                },
            )
            .unwrap(),
        )
        .unwrap();
    guard
        .accept_inbound(
            &Envelope::new(sid.clone(), 0, Message::Hello { role: Role::Helper }).unwrap(),
        )
        .unwrap();
    guard
        .accept_outbound(
            &Envelope::new(
                sid.clone(),
                1,
                Message::Request {
                    request: Request::Describe,
                },
            )
            .unwrap(),
        )
        .unwrap();
    let result = if broker {
        Reply::BrokerDescription {
            header: Box::new(header.clone()),
        }
    } else {
        Reply::Description {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            workspace_marker: WorkspaceMarker::from_bytes([3; 16]),
            os: Os::Linux,
            helper_principal: Principal::Posix {
                effective_uid: 1000,
                effective_gid: 1000,
            },
            programs: ProgramList::new(vec![ProgramId::new("install_target").unwrap()]).unwrap(),
        }
    };
    guard
        .accept_inbound(
            &Envelope::new(
                sid.clone(),
                1,
                Message::Response {
                    reply_to: 1,
                    result,
                },
            )
            .unwrap(),
        )
        .unwrap();
    (guard, sid)
}
#[test]
fn private_entry_wire_requires_broker_session_and_exact_ack() {
    use v2::*;
    let (p, _) = fixture();
    for private in [
        Request::PrepareOwnedEntry {
            preparation: Box::new(p.clone()),
        },
        Request::InspectOwnedEntry {
            lease: LeaseId::from_bytes([3; 16]),
            slot: p.slot.clone(),
            generation: None,
            sut: None,
        },
    ] {
        let (mut public, sid) = guard(false);
        let env = Envelope::new(
            sid,
            2,
            Message::Request {
                request: private.clone(),
            },
        )
        .unwrap();
        assert!(public.accept_outbound(&env).is_err());
        let (mut broker, sid) = guard(true);
        let env = Envelope::new(sid, 2, Message::Request { request: private }).unwrap();
        assert_eq!(
            Decoder::new().feed(&encode(&env).unwrap()).unwrap().frame,
            Some(env.clone())
        );
        broker.accept_outbound(&env).unwrap();
    }
    for changed in [false, true] {
        let (mut broker, sid) = guard(true);
        broker
            .accept_outbound(
                &Envelope::new(
                    sid.clone(),
                    2,
                    Message::Request {
                        request: Request::PrepareOwnedEntry {
                            preparation: Box::new(p.clone()),
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let mut ack = p.clone();
        if changed {
            ack.binding_digest = "ab".repeat(32);
        }
        let env = Envelope::new(
            sid,
            2,
            Message::Response {
                reply_to: 2,
                result: Reply::OwnedEntryPrepared {
                    preparation: Box::new(ack),
                },
            },
        )
        .unwrap();
        assert_eq!(broker.accept_inbound(&env).is_ok(), !changed);
    }
}
