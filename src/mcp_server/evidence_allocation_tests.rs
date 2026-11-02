// Included only in the existing editor test module, so its established
// thread-local System allocator meter remains the sole lib-test allocator.
#[test]
fn mcp_evidence_projection_never_allocates_complete_large_record() {
    use crate::mcp_server::{evidence, range_writer::RangeWriter, types::*};
    use crate::model::{
        CommandRecord, ProfileName, RunReport, ScenarioKind, ScenarioReport, StageReport,
    };

    fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
        struct StopTracking;
        impl Drop for StopTracking {
            fn drop(&mut self) {
                TRACK_ALLOCATIONS.with(|active| active.set(false));
            }
        }
        LARGEST_ALLOCATION.with(|largest| largest.set(0));
        TRACK_ALLOCATIONS.with(|active| active.set(true));
        let guard = StopTracking;
        let value = f();
        drop(guard);
        (value, LARGEST_ALLOCATION.with(Cell::get))
    }

    // Model/store/argument setup and the intentionally large source allocation
    // precede measurement; only actual consumer projection is measured.
    let mut report = RunReport::new("allocation-witness");
    let mut scenario = ScenarioReport::new("scenario", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("stage");
    let mut command = CommandRecord::new("large-command", "phase");
    command
        .argv
        .push(format!("{}TAIL", "λ\n\t🐈".repeat(500_000)));
    stage.commands.push(command);
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    let snapshot = evidence::ReportSnapshot {
        revision: "f".repeat(64),
        report,
        sampled_hint: 0,
    };
    let temp = tempfile::tempdir().unwrap();
    let store = crate::report::create(&temp.path().join("run")).unwrap();
    let instance = Instance::parse(&"a".repeat(32)).unwrap();
    let args: EvidenceArgs = serde_json::from_value(serde_json::json!({
        "server_instance":instance,"handle":format!("{}:1",instance.as_str()),"revision":snapshot.revision,
        "selector":{"kind":"record","record_id":"s/0/stage/0/command/0"},"limit":4096
    })).unwrap();
    let reply = ReplyContext {
        era: Era::Legacy,
        id: RpcId::Integer(1),
        instance,
    };
    let (page, projection_largest) =
        measured(|| evidence::evidence(&store, &snapshot, &args, &reply).unwrap());
    let EvidenceData::Record(page) = page else {
        panic!("record page required")
    };
    assert_eq!(page.bytes.len(), 4096);
    assert!(page.total_bytes.get() > 5_000_000);
    assert!(
        projection_largest <= 65_536,
        "evidence projection allocated {projection_largest} bytes"
    );

    let source = &snapshot.report.scenarios[0].stages[0].commands[0];
    let mut window = RangeWriter::new(2_000_001, 4096);
    let (_, largest) = measured(|| serde_json::to_writer(&mut window, source).unwrap());
    assert_eq!(window.total_bytes(), page.total_bytes.get());
    assert_eq!(window.bytes().len(), 4096);
    assert!(
        largest <= 4096,
        "range serialization allocated {largest} bytes"
    );
    eprintln!(
        "MCP evidence allocation witness: total={} retained={} projection_largest={projection_largest} range_largest={largest}",
        page.total_bytes.get(),
        window.bytes().len()
    );
}
