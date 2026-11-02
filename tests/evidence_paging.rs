#[path = "support/browsing.rs"]
mod browsing;
use theseus::{
    model::*,
    tui::{
        App, Tab,
        evidence::{Catalog, FilterIndex},
        paging::DETAIL_BYTES,
    },
};
#[test]
fn seventy_thousand_matches_and_last_record_are_reachable() {
    let mut app = App::offline(
        browsing::metadata_report(70_001, 0),
        "/tmp/synthetic".into(),
    );
    app.tab = Tab::Files;
    let catalog = Catalog::for_app(&app);
    assert_eq!(catalog.count(), 70_003);
    let index = FilterIndex::new(&catalog, "FILE-");
    assert_eq!(index.len(), 70_001);
    assert!(index.checkpoint_count() <= 65_536);
    let selected = index.get(&catalog, 70_000).unwrap();
    assert!(selected.record.text().contains("file-70000"));
    assert!(selected.key > 65_535);
    let window = index.summaries(&catalog, 69_980, usize::MAX, 120);
    assert_eq!(window.len(), 21);
    assert!(window.iter().all(|row| row.text.len() <= 120));
    assert_eq!(
        FilterIndex::new(&catalog, "")
            .summaries(&catalog, 0, usize::MAX, 120)
            .len(),
        64
    );
}
#[test]
fn large_unicode_field_is_paged_without_losing_tail_or_controls() {
    let mut report = browsing::metadata_report(1, 40_000);
    report.scenarios[0].stages[0].snapshots[0]
        .entries
        .get_mut("/app/file-00000")
        .unwrap()
        .link_target
        .as_mut()
        .unwrap()
        .push_str("\t\x1b🐈");
    let mut app = App::offline(report, "/tmp/synthetic".into());
    app.tab = Tab::Files;
    let catalog = Catalog::for_app(&app);
    let record = catalog.get(catalog.count() - 1).unwrap().record;
    let mut offset = 0;
    let mut text = String::new();
    let mut pages = 0;
    loop {
        let page = record.page(offset, DETAIL_BYTES).unwrap();
        assert!(page.text.len() <= DETAIL_BYTES);
        assert!(!page.text.contains('\x1b'));
        text.push_str(&page.text);
        pages += 1;
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert!(pages >= 3);
    assert!(text.contains("DETAIL_TAIL"));
    assert_eq!(text.matches('λ').count(), 20_000);
    assert!(record.page(0, 0).is_err());
    assert!(record.page(usize::MAX, DETAIL_BYTES).is_err());
    assert_eq!(FilterIndex::new(&catalog, "detail_tail").len(), 1);
}
#[test]
fn exact_log_reference_survives_ambiguous_paths_and_terminal_error_filter() {
    let mut report = browsing::metadata_report(0, 0);
    let stage = &mut report.scenarios[0].stages[0];
    let mut command = CommandRecord::new("one", "one");
    command.stdout = Some(LogReference {
        path: "logs/shared".into(),
        bytes: 1,
        truncated: false,
    });
    command.stderr = Some(LogReference {
        path: "logs/shared-extra".into(),
        bytes: 2,
        truncated: true,
    });
    stage.commands.push(command);
    let mut app = App::offline(report, "/tmp/synthetic".into());
    app.tab = Tab::Logs;
    app.terminal_error = Some("worker failed".into());
    let catalog = Catalog::for_app(&app);
    let index = FilterIndex::new(&catalog, "stderr");
    assert_eq!(index.len(), 2);
    assert!(
        index
            .get(&catalog, 0)
            .unwrap()
            .record
            .text()
            .contains("worker failed")
    );
    assert_eq!(
        index
            .get(&catalog, 1)
            .unwrap()
            .record
            .log_reference()
            .unwrap()
            .bytes,
        2
    );
}
#[test]
fn actual_controls_tabs_and_unicode_scalar_boundaries_are_lossless() {
    use theseus::tui::evidence::Record;
    let text = format!("{}\t\x1b\n🐈DETAIL_TAIL", "λ".repeat(20_000));
    let record = Record::Text(&text);
    let mut offset = 0;
    let mut output = String::new();
    loop {
        let p = record.page(offset, 17).unwrap();
        assert!(p.text.len() <= 17);
        output.push_str(&p.text);
        if let Some(next) = p.next_offset {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(output, theseus::tui::safe_text(&text).replace('\t', "    "));
    assert!(record.page(1, 17).is_err());
    assert!(record.page(0, DETAIL_BYTES + 1).is_err());
}
#[test]
fn fixture_is_completed_user_clean_with_requested_utf8_byte_count() {
    let report = browsing::metadata_report(2, 5);
    assert!(report.finished_at.is_some());
    assert_eq!(report.selected_profiles, vec![ProfileName::User]);
    assert_eq!(report.selected_scenarios, vec![ScenarioKind::Clean]);
    let entries = &report.scenarios[0].stages[0].snapshots[0].entries;
    assert_eq!(
        entries["/app/file-00001"].link_target.as_deref(),
        Some("λλxDETAIL_TAIL")
    );
}
#[test]
fn legacy_rendered_case_filter_including_final_sigma_is_preserved() {
    let mut report = browsing::metadata_report(0, 0);
    report.errors = vec!["ΟΣ".into(), "οσ".into(), "A\tB\x1bC".into()];
    report.scenarios.clear();
    let mut app = App::offline(report, "/tmp/synthetic".into());
    for needle in ["ος", "οσ", "a\tb�c"] {
        app.filter = needle.into();
        let expected: Vec<_> = app
            .report
            .as_ref()
            .unwrap()
            .errors
            .iter()
            .map(|s| theseus::tui::safe_text(s))
            .filter(|s| s.to_lowercase().contains(&needle.to_lowercase()))
            .collect();
        assert_eq!(app.items(), expected);
    }
}
#[test]
fn comparison_catalog_exposes_every_difference_and_full_source_detail() {
    use theseus::{
        comparison::*,
        snapshot::{ChangeKind, FileChange},
        tui::evidence::Record,
    };
    let manifest: SuiteManifest =
        toml::from_str(include_str!("../examples/comparisons/methods.toml")).unwrap();
    let report = ComparisonReport {
        sources: vec![ComparisonSource {
            variant_id: "source".into(),
            run_id: Some("λ".repeat(20_000) + "SOURCE_TAIL"),
            report_path: Some("sources/source/run/report.json".into()),
            recipe_digest: Some("digest".into()),
            image: Some("image".into()),
            image_id: Some("identity".into()),
            artifacts: vec![],
            outcome: RunOutcome::Passed,
            error: None,
        }],
        cells: vec![ComparisonCell {
            variant_id: "candidate".into(),
            profile: ProfileName::User,
            scenario: ScenarioKind::Clean,
            stage: "configured".into(),
            scope_id: manifest.scopes[0].id.clone(),
            comparison: ScopeComparison {
                status: ComparisonStatus::Different,
                diagnostics: vec!["CELL_DIAGNOSTIC".into()],
                differences: (0..70_001)
                    .map(|n| FileChange {
                        path: format!("entry-{n}"),
                        kind: ChangeKind::Added,
                        before: None,
                        after: None,
                    })
                    .collect(),
            },
        }],
        outcome: RunOutcome::Passed,
        diagnostics: vec!["SUITE_DIAGNOSTIC".into()],
    };
    let app = App::comparison_offline(
        LoadedComparison { manifest, report },
        "/tmp/suite's root".into(),
    );
    let catalog = Catalog::for_app(&app);
    let last = catalog.get(catalog.count() - 1).unwrap();
    assert!(last.record.text().contains("entry-70000"));
    assert!(matches!(last.record, Record::Change("Comparison", _)));
    let source = catalog
        .records()
        .find(|r| matches!(r.record, Record::Source(_)))
        .unwrap()
        .record;
    assert!(
        source
            .page(source.text().find('λ').unwrap() + 32_768, DETAIL_BYTES)
            .unwrap()
            .text
            .contains("SOURCE_TAIL")
    );
    let cleanup = catalog
        .records()
        .find(|r| matches!(r.record, Record::Cleanup(_)))
        .unwrap()
        .record;
    #[cfg(unix)]
    let expected_cleanup = "Recovery: theseus cleanup '/tmp/suite'\"'\"'s root'";
    #[cfg(windows)]
    let expected_cleanup = "Recovery: theseus cleanup '/tmp/suite''s root'";
    assert_eq!(cleanup.text(), expected_cleanup);
    assert_eq!(FilterIndex::new(&catalog, "entry-").len(), 70_001);
}
