//! Pure saved schema2 projections. No target or manager authority is consulted.
use super::model::*;
use crate::{
    model::{CheckOutcome, ResultCounts, RunOutcome},
    native::model::*,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

impl NativeReportVersion {
    pub fn consumer_scenarios(&self) -> &[NativeScenario] {
        match self {
            Self::CatalogV1(r) => &r.scenarios,
            Self::IntegrationV2(r) => &r.catalog.scenarios,
        }
    }
    pub fn consumer_outcome(&self) -> RunOutcome {
        match self {
            Self::CatalogV1(r) => r.outcome,
            Self::IntegrationV2(r) => r.outcome,
        }
    }
    pub fn consumer_counts(&self) -> ResultCounts {
        match self {
            Self::CatalogV1(r) => catalog_counts(&r.scenarios),
            Self::IntegrationV2(r) => integration_counts(r),
        }
    }
}
fn add_check(counts: &mut ResultCounts, outcome: CheckOutcome) {
    match outcome {
        CheckOutcome::Passed => counts.passed_checks += 1,
        CheckOutcome::Failed => counts.failed_checks += 1,
        CheckOutcome::Incomplete => counts.incomplete_checks += 1,
        CheckOutcome::Skipped => counts.skipped_checks += 1,
    }
}
pub fn catalog_counts(scenarios: &[NativeScenario]) -> ResultCounts {
    let mut counts = ResultCounts::default();
    for scenario in scenarios {
        for stage in &scenario.stages {
            counts.commands += stage.commands.len();
            for check in &stage.checks {
                add_check(&mut counts, check.outcome);
            }
        }
    }
    counts
}
pub fn integration_counts(report: &NativeIntegrationReport) -> ResultCounts {
    let mut counts = catalog_counts(&report.catalog.scenarios);
    for scenario in &report.integrations {
        for check in &scenario.checks {
            add_check(&mut counts, check.outcome);
        }
    }
    counts
}
#[derive(Debug, Clone, Copy)]
pub enum Selector {
    Overview,
    Services,
    Readiness,
    Checks,
    Dispositions,
    Desktop,
    NativeFiles,
    Commands,
    Identities,
    Failures,
}
pub fn evidence_page(
    report: &NativeIntegrationReport,
    selector: Selector,
    offset: usize,
    limit: usize,
) -> Result<crate::native::view::Page> {
    report.validate()?;
    ensure!(
        (1..=64).contains(&limit),
        "integration page limit must be 1..=64"
    );
    let mut records = Vec::new();
    let mut total = 0usize;
    let mut bytes = 0usize;
    let mut full = false;
    let mut push = |value: serde_json::Value| {
        if total >= offset && records.len() < limit && !full {
            let length = serde_json::to_vec(&value).map_or(usize::MAX, |b| b.len());
            if bytes.saturating_add(length) > 65536 {
                full = true;
            } else {
                bytes += length;
                records.push(value);
            }
        }
        total += 1;
    };
    visit_record_values(report, selector, &mut push)?;
    ensure!(
        offset <= total,
        "integration page offset exceeds record count"
    );
    ensure!(
        offset == total || !records.is_empty(),
        "integration record exceeds page byte budget"
    );
    let next = offset + records.len();
    Ok(crate::native::view::Page {
        records,
        offset,
        next_offset: (next < total).then_some(next),
    })
}
fn visit_record_values(
    report: &NativeIntegrationReport,
    selector: Selector,
    mut push: impl FnMut(serde_json::Value),
) -> Result<()> {
    match selector {
        Selector::Overview => push(
            serde_json::json!({"kind":report.kind,"schema":report.schema,"profile":report.profile,"run_id":report.run_id,"recipe_digest":report.recipe_digest,"core_recipe_digest":report.core_recipe_digest,"core_manifest_digest":report.core_manifest_digest,"integration_manifest_digest":report.integration_manifest_digest,"manifest_digest":report.manifest_digest,"summary":summary(report)}),
        ),
        Selector::NativeFiles | Selector::Commands | Selector::Identities | Selector::Failures => {
            match selector {
                Selector::Identities => {
                    for identity in &report.catalog.identities {
                        push(serde_json::to_value(identity)?);
                    }
                    for claim in &report.catalog.provider_claims {
                        push(serde_json::json!({"provenance":"provider_reported","claim":claim}));
                    }
                }
                Selector::Failures => {
                    for failure in &report.catalog.failures {
                        push(serde_json::json!({"scope":"run","failure":failure}));
                    }
                    for scenario in &report.catalog.scenarios {
                        for stage in &scenario.stages {
                            for failure in &stage.failures {
                                push(
                                    serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"failure":failure}),
                                );
                            }
                        }
                    }
                }
                _ => {
                    for scenario in &report.catalog.scenarios {
                        for stage in &scenario.stages {
                            if matches!(selector, Selector::NativeFiles) {
                                for file in &stage.native_files {
                                    push(
                                        serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"native_file":file}),
                                    );
                                }
                            } else {
                                for command in &stage.commands {
                                    push(
                                        serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"command":command}),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        Selector::Services
        | Selector::Desktop
        | Selector::Checks
        | Selector::Readiness
        | Selector::Dispositions => {
            for s in &report.integrations {
                match selector {
                    Selector::Services => {
                        for service in &s.services {
                            for observation in &service.observations {
                                push(
                                    serde_json::json!({"scenario_id":s.scenario_id,"slot":service.slot,"observation":observation}),
                                );
                            }
                            for action in &service.actions {
                                push(
                                    serde_json::json!({"scenario_id":s.scenario_id,"slot":service.slot,"action":action}),
                                );
                            }
                        }
                    }
                    Selector::Desktop => {
                        for desktop in &s.desktops {
                            for observation in &desktop.observations {
                                push(
                                    serde_json::json!({"scenario_id":s.scenario_id,"slot":desktop.slot,"observation":observation}),
                                );
                            }
                            for action in &desktop.actions {
                                push(
                                    serde_json::json!({"scenario_id":s.scenario_id,"slot":desktop.slot,"action":action}),
                                );
                            }
                        }
                    }
                    Selector::Checks | Selector::Readiness => {
                        for check in &s.checks {
                            if matches!(selector, Selector::Checks)
                                || matches!(
                                    check.predicate,
                                    IntegrationPredicate::Service(
                                        ServicePredicate::ApplicationReady
                                    ) | IntegrationPredicate::ApplicationReady
                                )
                            {
                                push(if matches!(selector, Selector::Readiness) {
                                    readiness_fact(report, check)?
                                } else {
                                    serde_json::to_value(check)?
                                });
                            }
                        }
                    }
                    Selector::Dispositions => push(
                        serde_json::json!({"scenario_id":s.scenario_id,"dispositions":s.dispositions}),
                    ),
                    _ => unreachable!(),
                }
            }
        }
    }
    Ok(())
}

/// A bounded UTF-8 detail slice of one logical record, independent of record size.
#[derive(Debug)]
pub struct DetailPage {
    pub bytes: Vec<u8>,
    pub byte_offset: usize,
    pub total_bytes: usize,
    pub next_byte_offset: Option<usize>,
    pub next_record_offset: Option<usize>,
}
struct DetailRange {
    start: usize,
    end: usize,
    total: usize,
    bytes: Vec<u8>,
}
impl std::io::Write for DetailRange {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let before = self.total;
        self.total = self
            .total
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("detail length overflow"))?;
        let start = self.start.max(before);
        let end = self.end.min(self.total);
        if start < end {
            self.bytes
                .extend_from_slice(&input[start - before..end - before]);
        }
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn detail_page(
    report: &NativeIntegrationReport,
    selector: Selector,
    record_offset: usize,
    byte_offset: usize,
    limit: usize,
) -> Result<DetailPage> {
    report.validate()?;
    ensure!(
        (4..=4096).contains(&limit),
        "native detail limit must be 4..=4096"
    );
    let mut selected = None;
    let mut records = 0;
    visit_record_values(report, selector, |value| {
        if records == record_offset {
            selected = Some(value);
        }
        records += 1;
    })?;
    ensure!(
        record_offset < records || (record_offset == 0 && records == 0),
        "native detail record exceeds count"
    );
    let value = selected.unwrap_or_else(|| serde_json::json!({"records":[]}));
    let start = byte_offset.saturating_sub(3);
    let end = byte_offset
        .checked_add(limit + 3)
        .ok_or_else(|| anyhow::anyhow!("detail offset overflow"))?;
    let mut range = DetailRange {
        start,
        end,
        total: 0,
        bytes: vec![],
    };
    serde_json::to_writer_pretty(&mut range, &value)?;
    ensure!(
        byte_offset <= range.total,
        "native detail byte offset exceeds record"
    );
    let mut from = byte_offset - start;
    while from > 0 && range.bytes.get(from).is_some_and(|b| b & 0xc0 == 0x80) {
        from -= 1;
    }
    let mut to = (from + limit).min(range.bytes.len());
    while to > from && range.bytes.get(to).is_some_and(|b| b & 0xc0 == 0x80) {
        to -= 1;
    }
    let bytes = range.bytes[from..to].to_vec();
    std::str::from_utf8(&bytes)?;
    let actual_start = start + from;
    let actual_end = start + to;
    ensure!(
        actual_end == range.total || !bytes.is_empty(),
        "native detail page made no progress"
    );
    Ok(DetailPage {
        bytes,
        byte_offset: actual_start,
        total_bytes: range.total,
        next_byte_offset: (actual_end < range.total).then_some(actual_end),
        next_record_offset: (record_offset + 1 < records).then_some(record_offset + 1),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispositionFlags {
    pub resources: usize,
    pub not_started: usize,
    pub complete: usize,
    pub unknown: usize,
    pub failed: usize,
}
/// Keep the exact checked observation provenance beside the readiness result.
pub fn readiness_fact(
    report: &NativeIntegrationReport,
    check: &IntegrationCheck,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(check)?;
    let observations = report
        .integrations
        .iter()
        .filter(|s| s.scenario_id == check.scenario_id)
        .flat_map(|s| &s.services)
        .flat_map(|s| &s.observations)
        .filter(|o| check.observation_ids.contains(&o.binding.id))
        .collect::<Vec<_>>();
    value["application_reports"] = serde_json::Value::Array(observations.iter().filter_map(|observation| {
        let readiness=observation.readiness.as_ref()?;
        readiness.response_digest.as_ref()?;
        Some(serde_json::json!({"observation_id":observation.binding.id,"provenance":"application_reported","application":observation.binding.slot,"value":{"artifact_sha256":readiness.artifact_sha256,"version_literal":readiness.version_literal,"data_sha256":readiness.data_sha256,"configuration_digest":readiness.configuration_digest,"initialized":readiness.initialized}}))
    }).collect());
    value["observations"] = serde_json::to_value(observations)?;
    Ok(value)
}
/// The private probe nonce never crosses a public evidence boundary.
pub fn readiness_probe_fact(probe: &ReadinessProbeRecord) -> serde_json::Value {
    use sha2::{Digest as _, Sha256};
    let challenge = &probe.challenge;
    let bytes = serde_json::to_vec(challenge).expect("typed readiness challenge serializes");
    serde_json::json!({"readiness_probe":{
        "challenge_record_sha256": format!("{:x}", Sha256::digest(bytes)),
        "challenge_digest":challenge.digest().expect("validated readiness challenge has canonical digest"),
        "challenge":{"ownership_id":challenge.ownership_id,"lease":challenge.lease,"slot":challenge.slot,"operation":challenge.operation,"resource_binding_digest":challenge.resource_binding_digest,"action_digest":challenge.action_digest,"expectation_digest":challenge.expectation_digest,"deadline_ms":challenge.deadline_ms},
        "expectation":probe.expectation,"expectation_digest":probe.expectation_digest,
        "observation_ids":probe.observation_ids,"completion":probe.completion
    }})
}
fn flags<'a>(states: impl Iterator<Item = &'a Disposition>) -> DispositionFlags {
    let mut f = DispositionFlags {
        resources: 0,
        not_started: 0,
        complete: 0,
        unknown: 0,
        failed: 0,
    };
    for state in states {
        f.resources += 1;
        match state {
            Disposition::NotStarted => f.not_started += 1,
            Disposition::Complete { .. } => f.complete += 1,
            Disposition::Unknown { .. } => f.unknown += 1,
            Disposition::Failed { .. } => f.failed += 1,
        }
    }
    f
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub schema: u32,
    pub profile: String,
    pub outcome: RunOutcome,
    pub services: usize,
    pub desktops: usize,
    pub checks: usize,
    pub first_required_failure: Option<String>,
    pub helper_worker: DispositionFlags,
    pub registration: DispositionFlags,
    pub manager_action: DispositionFlags,
    pub broker: DispositionFlags,
    pub primary: DispositionFlags,
    pub descendant_scope: DispositionFlags,
}
pub fn summary(report: &NativeIntegrationReport) -> Summary {
    let scenarios = &report.integrations;
    let preview = |s: &str| {
        let mut end = s.len().min(256);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_owned()
    };
    Summary {
        schema: report.schema,
        profile: report.profile.clone(),
        outcome: report.outcome,
        services: scenarios.iter().map(|s| s.services.len()).sum(),
        desktops: scenarios.iter().map(|s| s.desktops.len()).sum(),
        checks: scenarios.iter().map(|s| s.checks.len()).sum(),
        first_required_failure: scenarios
            .iter()
            .flat_map(|s| &s.checks)
            .filter(|c| c.origin != CheckOrigin::SafetyCleanup)
            .find(|c| c.outcome != CheckOutcome::Passed)
            .map(|c| preview(&format!("{}: {}", c.id, c.detail)))
            .or_else(|| {
                report.requirements.service.as_ref().and_then(|service| {
                    service.checks.iter().find_map(|required| {
                        let missing = required
                            .scenarios
                            .iter()
                            .filter(|kind| report.selected_scenarios.contains(kind))
                            .any(|kind| {
                                report
                                    .catalog
                                    .scenarios
                                    .iter()
                                    .find(|s| s.scenario == *kind)
                                    .and_then(|core| {
                                        scenarios.iter().find(|s| s.scenario_id == core.id)
                                    })
                                    .is_none_or(|s| !s.checks.iter().any(|c| c.id == required.id))
                            });
                        missing.then(|| {
                            preview(&format!("{}: required evidence missing", required.id))
                        })
                    })
                })
            }),
        helper_worker: flags(scenarios.iter().map(|s| &s.dispositions.helper_worker)),
        registration: flags(scenarios.iter().map(|s| &s.dispositions.registration)),
        manager_action: flags(scenarios.iter().map(|s| &s.dispositions.manager_action)),
        broker: flags(scenarios.iter().map(|s| &s.dispositions.broker)),
        primary: flags(scenarios.iter().map(|s| &s.dispositions.primary)),
        descendant_scope: flags(scenarios.iter().map(|s| &s.dispositions.descendant_scope)),
    }
}

/// Pure admission against the separately selected immutable target ceiling.
pub fn validate_authority(
    recipe: &super::recipe::ValidatedIntegrationRecipe,
    launch: &crate::native::launch::LaunchAuthority,
) -> Result<()> {
    let (core, integration) = launch.integration_components().ok_or_else(|| {
        anyhow::anyhow!("integration recipe requires integration launch authority")
    })?;
    let os = match recipe.core.recipe.os {
        crate::native::recipe::NativeOs::Linux => theseus_native_protocol::v2::Os::Linux,
        crate::native::recipe::NativeOs::Macos => theseus_native_protocol::v2::Os::Macos,
        crate::native::recipe::NativeOs::Windows => theseus_native_protocol::v2::Os::Windows,
    };
    ensure!(
        launch.binding().os == os
            && core.to_hex() == recipe.core.recipe.manifest_digest
            && integration == recipe.integration_manifest_digest
            && launch.binding().manifest_digest.to_hex() == recipe.full_manifest_digest()?,
        "integration recipe exceeds selected launch authority"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::ScenarioKind;
    pub(crate) fn report() -> NativeIntegrationReport {
        let slot = ResourceSlot::new("fixture_service").unwrap();
        let requirements = IntegrationRequirements {
            service: Some(ServiceRequirement {
                resource: slot.clone(),
                managed_by: ManagedBy::Installer,
                controls: RehearsalControls {
                    configured_activation: ActivationCause::RehearsalStart,
                    upgrade_precondition: RuntimePrecondition::Running,
                    reinstall_precondition: RuntimePrecondition::Running,
                    uninstall_precondition: RuntimePrecondition::Running,
                },
                definitions: vec![DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }],
                checks: vec![ServiceCheckRequirement {
                    id: "ready".into(),
                    scenarios: vec![ScenarioKind::Clean],
                    phase: IntegrationPhase::Exercised,
                    role: None,
                    artifact: None,
                    predicate: ServicePredicate::ApplicationReady,
                }],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 1000,
                },
            }),
            desktop: None,
        };
        let core = theseus_native_protocol::v2::Digest::parse(&"1".repeat(64)).unwrap();
        let integration = theseus_native_protocol::v2::Digest::parse(&"2".repeat(64)).unwrap();
        let pending = || Disposition::Unknown {
            reason: "original local owner pending".into(),
        };
        NativeIntegrationReport {
            kind: "theseus_native_report".into(),
            schema: 2,
            profile: super::super::PROFILE.into(),
            run_id: "integration-run".into(),
            recipe_digest: "3".repeat(64),
            core_recipe_digest: "4".repeat(64),
            core_manifest_digest: core.to_hex(),
            integration_manifest_digest: integration.to_hex(),
            manifest_digest: theseus_native_protocol::v2::integration_manifest_digest(
                core,
                integration,
            )
            .to_hex(),
            compiled_requirements_digest: requirements.digest().unwrap(),
            requirements,
            selected_scenarios: vec![ScenarioKind::Clean],
            catalog: CatalogRunEvidence {
                scenarios: vec![NativeScenario {
                    id: "clean".into(),
                    scenario: ScenarioKind::Clean,
                    ownership_id: "owner".into(),
                    stages: vec![],
                    outcome: RunOutcome::Incomplete,
                    dispositions: NativeDispositions::default(),
                }],
                identities: vec![],
                provider_claims: vec![],
                failures: vec![],
                outcome: RunOutcome::Incomplete,
            },
            integrations: vec![IntegrationScenarioEvidence {
                scenario_id: "clean".into(),
                ownership_id: "owner".into(),
                services: vec![ServiceEvidence {
                    slot: slot.clone(),
                    observations: vec![],
                    actions: vec![],
                }],
                desktops: vec![],
                checks: vec![IntegrationCheck {
                    id: "ready".into(),
                    scenario_id: "clean".into(),
                    phase: IntegrationPhase::Exercised,
                    slot,
                    role: None,
                    artifact: None,
                    predicate: IntegrationPredicate::Service(ServicePredicate::ApplicationReady),
                    origin: CheckOrigin::SutPostcondition,
                    sut_operation: None,
                    observation_ids: vec![],
                    outcome: CheckOutcome::Incomplete,
                    detail: "readiness response unavailable".into(),
                }],
                dispositions: IntegrationDispositions {
                    helper_worker: pending(),
                    registration: pending(),
                    manager_action: pending(),
                    broker: pending(),
                    primary: pending(),
                    descendant_scope: pending(),
                },
            }],
            outcome: RunOutcome::Incomplete,
        }
    }
    pub(crate) fn large_readiness_report() -> NativeIntegrationReport {
        let mut report = report();
        for i in 0..64 {
            let id = format!("large-observation-{i}");
            report.integrations[0].checks[0]
                .observation_ids
                .push(id.clone());
            report.integrations[0].services[0]
                .observations
                .push(BoundServiceObservation {
                    platform: None,
                    readiness: None,
                    binding: ObservationBinding {
                        id,
                        run_id: report.run_id.clone(),
                        scenario_id: "clean".into(),
                        ownership_id: "owner".into(),
                        lease: "a".repeat(32),
                        slot: ResourceSlot::new("fixture_service").unwrap(),
                        request_id: format!("large-request-{i}"),
                        action: None,
                        binding_digest: "b".repeat(64),
                        source_profile: "native_integration_v1".into(),
                        source_version: "fixture-v1".into(),
                        phase: IntegrationPhase::Exercised,
                        definition: None,
                        sequence: i,
                        started_ms: 0,
                        finished_ms: 1,
                        complete: false,
                    },
                    state: IntegrationProvenance::Unavailable {
                        reason: format!("unproved source {i}: {}", "é".repeat(1024)),
                    },
                });
        }
        let mut following = report.integrations[0].checks[0].clone();
        following.id = "ready-following".into();
        following.observation_ids.clear();
        report.integrations[0].checks.push(following);
        report.validate().unwrap();
        assert!(
            serde_json::to_vec(
                &readiness_fact(&report, &report.integrations[0].checks[0]).unwrap()
            )
            .unwrap()
            .len()
                > 65536
        );
        report
    }
    #[test]
    fn accepted_large_readiness_detail_is_byte_paged_without_losing_correlated_facts() {
        let report = large_readiness_report();
        let expected = readiness_fact(&report, &report.integrations[0].checks[0]).unwrap();
        let mut offset = 0;
        let mut bytes = vec![];
        let mut pages = 0;
        loop {
            let page = detail_page(&report, Selector::Readiness, 0, offset, 4096).unwrap();
            assert!(page.bytes.len() <= 4096);
            assert_eq!(page.byte_offset, offset);
            assert_eq!(page.next_record_offset, Some(1));
            std::str::from_utf8(&page.bytes).unwrap();
            bytes.extend(page.bytes);
            pages += 1;
            if let Some(next) = page.next_byte_offset {
                assert!(next > offset);
                offset = next;
            } else {
                assert_eq!(bytes.len(), page.total_bytes);
                break;
            }
        }
        assert!(
            pages > 16,
            "the accepted record exceeds the old 65536-byte budget"
        );
        let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual["observations"].as_array().unwrap().len(), 64);
        assert_eq!(actual["observation_ids"].as_array().unwrap().len(), 64);
        assert_eq!(
            actual["observations"][63]["state"]["provenance"],
            "unavailable"
        );
        let next = detail_page(&report, Selector::Readiness, 1, 0, 4096).unwrap();
        assert!(next.next_record_offset.is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&next.bytes).unwrap()["id"],
            "ready-following"
        );
    }
    #[test]
    fn unknown_required_readiness_stays_incomplete_and_missing_check_cannot_pass() {
        let mut report = report();
        report.validate().unwrap();
        let page = evidence_page(&report, Selector::Readiness, 0, 1).unwrap();
        assert_eq!(page.records[0]["outcome"], "incomplete");
        assert_eq!(summary(&report).helper_worker.unknown, 1);
        assert!(
            summary(&report)
                .first_required_failure
                .unwrap()
                .contains("readiness response unavailable")
        );
        report.integrations[0].checks.clear();
        assert!(
            evidence_page(&report, Selector::Readiness, 0, 1)
                .unwrap()
                .records
                .is_empty()
        );
        assert_eq!(summary(&report).outcome, RunOutcome::Incomplete);
        assert!(
            summary(&report)
                .first_required_failure
                .unwrap()
                .contains("required evidence missing")
        );
        report.outcome = RunOutcome::Passed;
        assert!(evidence_page(&report, Selector::Readiness, 0, 1).is_err());
    }
    #[test]
    fn known_failed_postcondition_stays_failed_and_status_is_trace_independent() {
        let mut report = report();
        report.integrations[0].checks[0].outcome = CheckOutcome::Failed;
        report.outcome = RunOutcome::Failed;
        assert_eq!(
            evidence_page(&report, Selector::Checks, 0, 1)
                .unwrap()
                .records[0]["outcome"],
            "failed"
        );
        let before = serde_json::to_vec(&summary(&report)).unwrap();
        for i in 0..64 {
            report.integrations[0].services[0]
                .observations
                .push(BoundServiceObservation {
                    platform: None,
                    readiness: None,
                    binding: ObservationBinding {
                        id: format!("observation-{i}"),
                        run_id: report.run_id.clone(),
                        scenario_id: "clean".into(),
                        ownership_id: "owner".into(),
                        lease: "a".repeat(32),
                        slot: ResourceSlot::new("fixture_service").unwrap(),
                        request_id: format!("request-{i}"),
                        action: None,
                        binding_digest: "b".repeat(64),
                        source_profile: "qualified_manager".into(),
                        source_version: "fixture-v1".into(),
                        phase: IntegrationPhase::Exercised,
                        definition: None,
                        sequence: i,
                        started_ms: 0,
                        finished_ms: 1,
                        complete: false,
                    },
                    state: IntegrationProvenance::Unavailable {
                        reason: "manager generation unproved".into(),
                    },
                });
        }
        assert_eq!(before, serde_json::to_vec(&summary(&report)).unwrap());
        let first = evidence_page(&report, Selector::Services, 0, 1).unwrap();
        assert_eq!(first.next_offset, Some(1));
        assert_eq!(
            first.records[0]["observation"]["state"]["provenance"],
            "unavailable"
        );
        assert!(evidence_page(&report, Selector::Services, 0, 65).is_err());
    }
    #[test]
    fn public_readiness_probe_exposes_correlation_and_digest_without_private_nonce() {
        let expectation = ReadinessExpectation {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
            artifact_length: 1,
            configuration_digest: "9".repeat(64),
            artifact_digest: "d".repeat(64),
            version: "fixture-v1".into(),
            data_digest: "e".repeat(64),
            initialized: true,
        };
        let expectation_digest = expectation.digest().unwrap();
        let probe = ReadinessProbeRecord {
            challenge: ReadinessChallenge {
                resource_binding_digest: "7".repeat(64),
                action_digest: "8".repeat(64),
                expectation_digest: expectation_digest.clone(),
                ownership_id: "owner".into(),
                lease: "a".repeat(32),
                slot: ResourceSlot::new("fixture_service").unwrap(),
                operation: "b".repeat(32),
                nonce: "c".repeat(32),
                deadline_ms: 1000,
            },
            expectation,
            expectation_digest,
            observation_ids: vec!["readiness-1".into()],
            completion: Disposition::Unknown {
                reason: "response pending".into(),
            },
        };
        let value = readiness_probe_fact(&probe);
        assert!(value["readiness_probe"]["challenge"].get("nonce").is_none());
        assert!(
            !serde_json::to_string(&value)
                .unwrap()
                .contains(&probe.challenge.nonce)
        );
        assert_eq!(
            value["readiness_probe"]["challenge_record_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(
            value["readiness_probe"]["challenge"]["operation"],
            probe.challenge.operation
        );
    }
}
