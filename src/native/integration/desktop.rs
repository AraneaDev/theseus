//! Desktop hooks compose the same ordinary lifecycle and retained projector.
use super::{ValidatedIntegrationRecipe, model::*};
use crate::{
    engine::{
        common::{self, LifecycleBackend, sealed},
        domain::{StoredJournal, StoredRun},
    },
    model::{CheckOutcome, Checkpoint, ScenarioKind, StepName},
    native::{adapter::NativeDriver, model::Disposition},
};
use anyhow::{Result, bail, ensure};
use std::time::{Duration, Instant};
use theseus_native_client::Action;
use theseus_native_protocol::v2::{self, broker};

pub struct DesktopDriver<'a, 'b, 's> {
    core: &'a mut NativeDriver<'b, 's>,
    hooks: DesktopHooks,
}
pub(crate) struct DesktopHooks {
    header: broker::IntegrationBrokerHeader,
    manifest: v2::IntegrationManifest,
    requirement: DesktopRequirement,
    mode: broker::DesktopMode,
    receipt: Option<broker::BrokerBindingReceipt>,
    entry: Option<broker::DesktopObservation>,
    activation: Option<broker::DesktopRequest>,
    close: Option<broker::DesktopRequest>,
    state: Option<broker::BrokerState>,
    sequence: u32,
    started: Instant,
    forward: bool,
    deferred_failure: Option<anyhow::Error>,
    scenario: ScenarioKind,
    first_install: bool,
    after_reinstall: bool,
    definition: Option<v2::DesktopDefinition>,
}
impl DesktopHooks {
    pub(crate) fn forward_active(&self) -> bool {
        self.forward
    }
}

mod admission;
mod effects;
mod observe;

#[cfg(test)]
use admission::selected_desktop_mode;
pub(crate) use admission::{desktop_readiness_completion, validate_admission};
use admission::{
    desktop_readiness_expectation, nonce, operation, registration_removal_disposition,
};

impl<'a, 'b, 's> DesktopDriver<'a, 'b, 's> {
    pub(crate) fn new_for_scenario(
        core: &'a mut NativeDriver<'b, 's>,
        recipe: &ValidatedIntegrationRecipe,
        header: broker::IntegrationBrokerHeader,
        scenario: ScenarioKind,
    ) -> Result<Self> {
        let manifest = core
            .session
            .integration_description()
            .ok_or_else(|| anyhow::anyhow!("desktop helper description missing"))?
            .manifest
            .as_ref()
            .clone();
        let requirement = recipe
            .recipe
            .integrations
            .desktop
            .clone()
            .ok_or_else(|| anyhow::anyhow!("desktop requirements missing"))?;
        let mode = validate_admission(recipe, &manifest, &header, scenario)?;
        core.session
            .require_linux_gio_admission(&manifest, &header)?;
        Ok(Self {
            core,
            hooks: DesktopHooks {
                header,
                manifest,
                requirement,
                mode,
                receipt: None,
                entry: None,
                activation: None,
                close: None,
                state: None,
                sequence: 0,
                started: Instant::now(),
                forward: true,
                deferred_failure: None,
                scenario,
                first_install: true,
                after_reinstall: false,
                definition: None,
            },
        })
    }
    fn desktop_manifest(&self) -> &broker::DesktopManifest {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.desktop)
            .unwrap_or_else(|| self.hooks.manifest.desktop.as_ref().unwrap())
    }
    fn readiness_manifest(&self) -> &v2::ReadinessDefinitionExpectation {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.readiness)
            .unwrap_or_else(|| self.hooks.manifest.desktop_readiness.as_ref().unwrap())
    }
    fn executable_manifest(&self) -> &v2::DesktopExecutable {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.executable)
            .unwrap_or_else(|| self.hooks.manifest.desktop_executable.as_ref().unwrap())
    }
    pub(crate) fn resume(core: &'a mut NativeDriver<'b, 's>, hooks: DesktopHooks) -> Self {
        Self { core, hooks }
    }
    pub(crate) fn into_hooks(self) -> DesktopHooks {
        self.hooks
    }
    pub(crate) fn prepare_hooks(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            scenario == self.hooks.scenario,
            "desktop scenario differs from exact admission"
        );
        self.bind()?;
        self.inspect(IntegrationPhase::Baseline)
    }
    pub(crate) fn finish_hooks(&mut self, result: Result<()>) -> Result<()> {
        match (self.hooks.deferred_failure.take(), result) {
            (None, result) => result,
            (Some(primary), Ok(())) => Err(primary),
            (Some(primary), Err(secondary)) => Err(anyhow::Error::new(DesktopCleanupFailure {
                primary,
                secondary,
            })),
        }
    }
    pub fn run(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            scenario == self.hooks.scenario,
            "desktop scenario differs from exact admission"
        );
        self.core.prepare(scenario)?;
        self.prepare_hooks(scenario)?;
        let result = common::run_common(self, scenario);
        self.finish_hooks(result)
    }
    fn evidence(&mut self) -> Result<&mut IntegrationScenarioEvidence> {
        let id = self.core.resource.resource().ownership_id.clone();
        let StoredRun::NativeIntegration(report) = self.core.projector.report_mut() else {
            bail!("desktop requires schema2 report")
        };
        report
            .integrations
            .iter_mut()
            .find(|s| s.ownership_id == id)
            .ok_or_else(|| anyhow::anyhow!("desktop scenario missing"))
    }
}
impl sealed::Sealed for DesktopDriver<'_, '_, '_> {}
impl DesktopDriver<'_, '_, '_> {
    pub(crate) fn before_step_hook(&mut self, step: StepName) -> Result<()> {
        let phase = match step {
            StepName::Upgrade => Some(IntegrationPhase::BeforeUpgrade),
            StepName::Reinstall => Some(IntegrationPhase::BeforeReinstall),
            StepName::Uninstall => Some(IntegrationPhase::BeforeUninstall),
            _ => None,
        };
        if let Some(phase) = phase
            && let Err(error) = self.close_original(phase)
        {
            self.hooks.forward = false;
            self.hooks.deferred_failure = Some(match self.hooks.deferred_failure.take() {
                Some(primary) => anyhow::Error::new(DesktopCleanupFailure {
                    primary,
                    secondary: error,
                }),
                None => error,
            });
            let scenario_id = self.core.resource.resource().scenario_id.clone();
            let slot = self.hooks.requirement.resource.clone();
            let lifetime = self.hooks.requirement.lifetime;
            let definition = self
                .hooks
                .definition
                .as_ref()
                .map(|d| DefinitionKey::from_desktop_wire(d.key));
            let id = desktop_scenario_check_specs(&self.hooks.requirement, self.hooks.scenario)
                .into_iter()
                .find(|(_, p, predicate, _)| {
                    *p == phase && matches!(predicate, IntegrationPredicate::Lifetime(_))
                })
                .map(|s| s.0)
                .unwrap_or_else(|| "desktop-original-lifetime".into());
            let evidence = self.evidence()?;
            if !evidence.checks.iter().any(|c| c.id == id) {
                evidence.checks.push(IntegrationCheck {id,scenario_id,phase,slot,role:definition.as_ref().map(|d|d.role),artifact:definition.as_ref().map(|d|d.artifact),predicate:IntegrationPredicate::Lifetime(lifetime),origin:CheckOrigin::RehearsalControl,sut_operation:None,observation_ids:vec![],outcome:CheckOutcome::Incomplete,detail:"original desktop close/lifetime/local retirement unresolved; forward SUT mutation blocked and declared Uninstall retained".into()});
            }
            self.core.projector.save_report()?;
        }
        Ok(())
    }
    pub(crate) fn after_step_hook(&mut self, step: StepName) -> Result<()> {
        if step == StepName::Reinstall && self.core.forward_active() {
            self.hooks.after_reinstall = true;
        }
        if step == StepName::Exercise && self.core.forward_active() && self.hooks.forward {
            self.readiness(
                super::driver::exercise_phase(self.hooks.scenario, self.hooks.after_reinstall),
                false,
            )?;
        }
        Ok(())
    }
    pub(crate) fn checkpoint_hook(
        &mut self,
        checkpoint: Checkpoint,
        _configured: bool,
    ) -> Result<()> {
        if (!self.core.forward_active() || !self.hooks.forward)
            && checkpoint != Checkpoint::Uninstalled
        {
            return Ok(());
        }
        match checkpoint {
            Checkpoint::BaseInstalled => {
                self.inspect(IntegrationPhase::BaseInstalled)?;
                self.hooks.first_install = false;
                Ok(())
            }
            Checkpoint::TargetInstalled if self.hooks.first_install => {
                self.inspect(IntegrationPhase::TargetInstalledInitial)?;
                self.hooks.first_install = false;
                Ok(())
            }
            Checkpoint::Configured => {
                if !self.hooks.manifest.desktop_definitions.is_empty() {
                    self.inspect(IntegrationPhase::ConfiguredBeforeActivation)?;
                }
                if self.hooks.forward {
                    self.activate()?;
                    self.readiness(IntegrationPhase::Configured, true)?;
                }
                Ok(())
            }
            Checkpoint::TargetInstalled | Checkpoint::Reinstalled => {
                let phase = if checkpoint == Checkpoint::Reinstalled {
                    IntegrationPhase::Reinstalled
                } else {
                    IntegrationPhase::TargetInstalledAfterUpgrade
                };
                self.inspect(phase)?;
                if self.hooks.forward {
                    self.activate()?;
                    self.readiness(phase, true)?;
                }
                Ok(())
            }
            Checkpoint::Uninstalled => self.inspect(IntegrationPhase::Uninstalled),
            _ => bail!("unsupported ordinary desktop checkpoint"),
        }
    }
}

impl LifecycleBackend for DesktopDriver<'_, '_, '_> {
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        if !self.hooks.forward && step != StepName::Uninstall {
            return Ok(());
        }
        self.before_step_hook(step)?;
        if !self.hooks.forward && step != StepName::Uninstall {
            return Ok(());
        }
        self.core.step(step, name)?;
        self.after_step_hook(step)
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        if !self.hooks.forward && checkpoint != Checkpoint::Uninstalled {
            return Ok(());
        }
        self.core.checkpoint(checkpoint, configured)?;
        self.checkpoint_hook(checkpoint, configured)
    }
    fn before_reinstall(&mut self) -> Result<()> {
        if self.hooks.forward && self.core.forward_active() {
            self.core.before_reinstall()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct DesktopCleanupFailure {
    primary: anyhow::Error,
    secondary: anyhow::Error,
}
impl std::fmt::Display for DesktopCleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::hint::black_box(&self.secondary);
        f.write_str("original desktop cleanup and subsequent SUT cleanup both failed")
    }
}
impl std::error::Error for DesktopCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

#[cfg(test)]
pub(crate) mod readiness_role_review_tests {
    use super::*;
    pub(crate) fn manifest(role: &str, artifact: &str) -> v2::IntegrationManifest {
        let expectation = v2::ReadinessDefinitionExpectation {
            role: role.into(),
            artifact: artifact.into(),
            artifact_length: 123,
            artifact_sha256: "33".repeat(32),
            version_literal: "target\n".into(),
            data_sha256: "44".repeat(32),
            configuration_digest: "55".repeat(32),
        };
        let manifest:v2::IntegrationManifest=serde_json::from_value(serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":expectation,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})).unwrap();
        manifest.validate().unwrap();
        let integration = manifest.digest().unwrap();
        assert_ne!(
            v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
                integration
            )
            .as_bytes(),
            &[0; 32]
        );
        manifest
    }
    #[test]
    fn desktop_readiness_preserves_complete_admitted_role_and_artifact_digest() {
        for (role, artifact) in [
            ("target", "target"),
            ("base", "base"),
            ("configured", "target"),
        ] {
            let manifest = manifest(role, artifact);
            let original = manifest.desktop_readiness.as_ref().unwrap();
            let saved = desktop_readiness_expectation(original).unwrap();
            assert_eq!(
                saved.wire(),
                *original,
                "an admitted tuple must not be relabeled after activation"
            );
            assert_eq!(
                saved.digest().unwrap(),
                manifest.desktop.as_ref().unwrap().expectation_digest
            );
        }
    }
}

#[cfg(test)]
mod native_selection_tests {
    use super::*;
    #[test]
    fn selected_native_requirement_matches_exact_platform_ceiling() {
        let definition = v2::NativeDesktopDefinition::WindowsShortcut {
            bootstrap: v2::NativeBootstrap::TheseusFixtureReadyBootstrapV1,
            lifetime: broker::NativeLifetimeRequirement::OwnedPrimaryExited,
        };
        assert_eq!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .unwrap(),
            broker::DesktopMode::NativeActivation
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::ActivationInstanceTerminated,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedProcessTreeSettled,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                None
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable, DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .is_err()
        );
        assert_eq!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable],
                LifetimeRequirement::OwnedProcessTreeSettled,
                None
            )
            .unwrap(),
            broker::DesktopMode::OwnedExecutable
        );
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../tests/support/native_desktop_app.rs"]
mod desktop_app_fixture;

#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "../../../tests/support/native_desktop_installer.rs"]
mod desktop_installer_fixture;

#[cfg(test)]
mod combined_registration_tests {
    use super::*;
    #[test]
    fn desktop_absence_cannot_clear_independent_service_failure_or_uncertainty() {
        let complete = Disposition::Complete {
            receipt: "exact removal".into(),
        };
        for service in [
            Disposition::Unknown {
                reason: "service removal unproved".into(),
            },
            Disposition::Failed {
                reason: "service leaked registration".into(),
            },
            Disposition::NotStarted,
        ] {
            assert_eq!(
                registration_removal_disposition(Some(&service), complete.clone()),
                service
            );
        }
        let failed = Disposition::Failed {
            reason: "desktop entry leaked".into(),
        };
        assert_eq!(
            registration_removal_disposition(Some(&complete), failed.clone()),
            failed
        );
        assert_eq!(
            registration_removal_disposition(None, complete.clone()),
            complete
        );
    }
}

#[cfg(test)]
#[path = "desktop_admission_tests.rs"]
mod native_ordinary_admission_tests;

#[cfg(all(test, feature = "native-integration-test-support"))]
mod macos_owned_admission_tests {
    use super::*;
    fn owned_fixture() -> (
        ValidatedIntegrationRecipe,
        v2::IntegrationManifest,
        serde_json::Value,
    ) {
        let mut recipe = ValidatedIntegrationRecipe::decode(
            include_bytes!("../../../examples/native/service-recipe.example.json"),
            "pure-recipe.json".into(),
        )
        .unwrap();
        recipe.recipe.integrations.service = None;
        recipe.recipe.integrations.desktop = Some(DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedPrimaryExited,
        });
        recipe.core.recipe.target.length = 123;
        recipe.core.recipe.target.sha256 = "33".repeat(32);
        let manifest = readiness_role_review_tests::manifest("target", "target");
        let raw = serde_json::json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":"11".repeat(32),"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"supplied","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]});
        (recipe, manifest, raw)
    }
    #[test]
    fn macos_owned_primary_admission_refuses_without_changing_other_platforms() {
        let (mut recipe, manifest, raw) = owned_fixture();
        for os in ["linux", "windows", "macos"] {
            let mut raw = raw.clone();
            raw["os"] = os.into();
            if os == "macos" {
                raw["display"] = serde_json::json!({"kind":"mac_os_gui","domain":"gui/1000"});
            }
            if os == "windows" {
                raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
                raw["broker_endpoint"] =
                    serde_json::json!({"kind":"named_pipe","path":r"\\.\pipe\theseus.fixture"});
                for key in ["broker_principal", "helper_principal"] {
                    raw[key] =
                        serde_json::json!({"kind":"windows","token_user_sid":"S-1-5-21-1000"});
                }
            }
            let mut header: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
            header.validate().unwrap();
            let selected = validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean);
            if os != "macos" {
                assert_eq!(selected.unwrap(), broker::DesktopMode::OwnedExecutable);
                continue;
            }
            let error = selected.expect_err("macOS OwnedPrimary cannot qualify product release");
            assert!(
                error
                    .to_string()
                    .contains("OwnedExecutable lifetime on macOS")
            );
            assert!(error.to_string().contains("NativeActivation"));
            let requirement = recipe.recipe.integrations.desktop.as_mut().unwrap();
            requirement.modes = vec![DesktopMode::NativeActivation];
            requirement.lifetime = LifetimeRequirement::ActivationInstanceTerminated;
            header.allowed_modes = vec![broker::DesktopMode::NativeActivation];
            header.macos_security_session_id = Some(1);
            let mut raw = serde_json::to_value(&manifest).unwrap();
            raw["desktop_native"] = serde_json::json!({"kind":"macos_bundle","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"activation_instance_terminated","app_leaf":"Fixture.app","bundle_identifier":"org.theseus.fixture","executable_leaf":"app","info_plist":{"length":128,"sha256":"22".repeat(32)}});
            raw["desktop"]["allowed_modes"] = serde_json::json!(["native_activation"]);
            raw["desktop"]["entry"] = "Fixture.app".into();
            raw["desktop_executable"]["root"] = "/fixture/entries".into();
            raw["desktop_executable"]["relative_path"] = "Fixture.app/Contents/MacOS/app".into();
            raw["desktop_executable"]["arguments"][0] =
                "/fixture/entries/Fixture.app/Contents/MacOS/app".into();
            raw["artifact_slots"][0]["relative_path"] = "Fixture.app/Contents/Info.plist".into();
            raw["desktop_capabilities"] =
                serde_json::json!(["inspect_desktop", "native_activation", "request_close"]);
            let manifest = serde_json::from_value(raw).unwrap();
            assert_eq!(
                validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).unwrap(),
                broker::DesktopMode::NativeActivation
            );
        }
    }

    #[test]
    fn explicit_owned_primary_declaration_is_required_and_platform_bound() {
        let (recipe, mut manifest, raw) = owned_fixture();
        manifest.desktop_owned_lifetime =
            Some(broker::OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        for os in ["macos", "linux"] {
            let mut raw = raw.clone();
            raw["os"] = os.into();
            if os == "macos" {
                raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
            }
            let header: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
            let selected = validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean);
            if os == "macos" {
                assert_eq!(selected.unwrap(), broker::DesktopMode::OwnedExecutable);
            } else {
                assert!(
                    selected.is_err(),
                    "Mac declaration cannot grant another platform authority"
                );
            }
        }
    }

    #[test]
    fn interactive_owned_windows_requires_native_entry_declaration() {
        let (recipe, manifest, mut raw) = owned_fixture();
        raw["os"] = "windows".into();
        raw["display"] = serde_json::json!({"kind":"windows_interactive","session_id":1,"window_station":"WinSta0","desktop":"Default"});
        let slash = char::from(92);
        let pipe = format!("{slash}{slash}.{slash}pipe{slash}theseus.fixture");
        raw["broker_endpoint"] = serde_json::json!({"kind":"named_pipe","path":pipe});
        for key in ["broker_principal", "helper_principal"] {
            raw[key] = serde_json::json!({"kind":"windows","token_user_sid":"S-1-5-21-1000"});
        }
        let header: broker::IntegrationBrokerHeader = serde_json::from_value(raw.clone()).unwrap();
        assert!(
            validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).is_err(),
            "pinned source bytes are not actual native shortcut qualification"
        );
        raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
        let source: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
        assert_eq!(
            validate_admission(&recipe, &manifest, &source, ScenarioKind::Clean).unwrap(),
            broker::DesktopMode::OwnedExecutable
        );
    }
}
