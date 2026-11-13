//! Integration and lease recovery publication.
use super::*;

impl RunProjector<'_> {
    pub(crate) fn publish_integration_recovery(
        &mut self,
        record: crate::native::integration::model::IntegrationRecoveryRecord,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        ensure!(
            j.ownership
                .iter()
                .any(|o| o.ownership_id == record.ownership_id),
            "integration recovery lacks original owner"
        );
        ensure!(
            !j.integration_recoveries
                .iter()
                .any(|r| r.recovery_id == record.recovery_id),
            "integration recovery already published"
        );
        let originals = j
            .integration_actions
            .iter()
            .filter(|a| a.ownership_id == record.ownership_id)
            .map(|a| crate::native::integration::model::ActionReference {
                operation: a.operation.clone(),
                request_digest: a.request_digest.clone(),
            })
            .collect::<Vec<_>>();
        ensure!(
            record.broker_effect_baselines
                == crate::native::integration::model::original_broker_effect_baselines(
                    j,
                    &record.ownership_id
                )?,
            "recovery desktop terminal baseline differs from original publication knowledge"
        );
        ensure!(
            record.original_actions == originals
                && record.queries.is_empty()
                && record.broker_observations.is_empty()
                && record.broker_release.is_none(),
            "integration recovery original action set changed"
        );
        j.integration_recoveries.push(record);
        self.save_journal()
    }
    pub(crate) fn record_integration_query(
        &mut self,
        recovery_id: &str,
        query: crate::native::integration::model::OriginalQueryOutcome,
    ) -> Result<()> {
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?;
        ensure!(
            record.original_actions.contains(&query.original)
                && !record.queries.iter().any(|q| q.original == query.original),
            "integration recovery query identity changed or duplicated"
        );
        let action = j
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == record.ownership_id
                    && a.operation == query.original.operation
                    && a.request_digest == query.original.request_digest
            })
            .ok_or_else(|| {
                anyhow::anyhow!("integration recovery query lacks exact original action")
            })?;
        match query.view {
            OriginalQueryView::CachedTerminal => {
                validate_cached_terminal(j, action, &query.outcome)?;
                crate::native::integration::model::validate_original_query_broker_receipts(
                    j,
                    &record.ownership_id,
                    &query,
                )?;
            }
            OriginalQueryView::NonterminalUnknown => ensure!(
                matches!(query.outcome, IntegrationOutcome::Unknown { .. }),
                "nonterminal recovery query cannot claim a terminal outcome"
            ),
        }
        j.integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .expect("published recovery checked")
            .queries
            .push(query);
        self.save_journal()
    }
    pub(crate) fn record_broker_recovery_observation(
        &mut self,
        recovery_id: &str,
        observation: crate::native::integration::model::RecoveredBrokerObservation,
    ) -> Result<()> {
        use crate::native::integration::model::*;
        let StoredJournal::NativeIntegration(j) = &self.journal else {
            bail!("broker recovery requires schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("broker recovery not published"))?;
        let owner = record.ownership_id.clone();
        let action = j
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == owner
                    && a.operation == observation.original.operation
                    && a.request_digest == observation.original.request_digest
            })
            .ok_or_else(|| anyhow::anyhow!("broker state lacks original action"))?;
        let IntegrationRequest::Desktop { request, .. } = &action.request else {
            bail!("broker state borrowed service authority")
        };
        ensure!(
            request.identity()? == observation.identity,
            "broker recovery identity changed"
        );
        validate_original_broker_effect(action, &observation.state)?;
        if let Some(baseline) = record
            .broker_effect_baselines
            .iter()
            .find(|b| b.original == observation.original)
        {
            validate_broker_effect_terminal(baseline.terminal.as_ref(), &observation.state)?;
        }
        let StoredRun::NativeIntegration(report) = &self.report else {
            bail!("broker recovery requires original integration report")
        };
        validate_original_broker_report_progress(report, action, &observation.state)?;
        if observation.source == BrokerObservationSource::Cancel {
            ensure!(
                matches!(
                    request.action,
                    theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
                ),
                "Cancel may control only original activation"
            );
        }
        if let Some(previous) = j
            .integration_recoveries
            .iter()
            .rev()
            .filter(|r| r.ownership_id == owner)
            .flat_map(|r| r.broker_observations.iter().rev())
            .find(|o| o.original == observation.original)
        {
            validate_broker_progress(&previous.state, &observation.state)?;
        }
        if record
            .broker_observations
            .iter()
            .rev()
            .find(|o| o.original == observation.original)
            .is_some_and(|o| o.state == observation.state)
        {
            return Ok(());
        }
        ensure!(
            record.broker_observations.len() < 64 * record.original_actions.len().max(1),
            "broker recovery state budget exceeded"
        );
        let state = observation.state.clone();
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            unreachable!()
        };
        j.integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .unwrap()
            .broker_observations
            .push(observation);
        self.save_journal()?;
        if let StoredRun::NativeIntegration(report) = &mut self.report
            && let Some(evidence) = report
                .integrations
                .iter_mut()
                .find(|s| s.ownership_id == owner)
        {
            use theseus_native_protocol::v2::broker::*;
            evidence.dispositions.primary = if matches!(
                state.primary,
                BrokerPrimary::Exited | BrokerPrimary::NotCreated
            ) {
                Disposition::Complete {receipt:"original broker observed original primary exited or never created during recovery".into()}
            } else {
                Disposition::Unknown {
                    reason: "original desktop primary exit remains unproved".into(),
                }
            };
            evidence.dispositions.descendant_scope = if matches!(
                state.descendants,
                BrokerDescendants::Joined | BrokerDescendants::NotCreated
            ) && state.cleanup_complete
            {
                Disposition::Complete {receipt:"original broker observed original scope joined or never created and IPC settled during recovery".into()}
            } else {
                Disposition::Unknown {
                    reason: "original desktop scope/IPC settlement remains unproved".into(),
                }
            };
            if !settled_original_broker_state(&state) {
                evidence.dispositions.broker = Disposition::Unknown {
                    reason: "original broker lifetime, effect or quarantine remains unresolved"
                        .into(),
                };
            }
        }
        self.save_report()
    }
    pub(crate) fn record_broker_recovery_release(
        &mut self,
        recovery_id: &str,
        release: crate::native::integration::model::RecoveredBrokerRelease,
    ) -> Result<()> {
        use crate::native::integration::model::*;
        let StoredJournal::NativeIntegration(j) = &self.journal else {
            bail!("broker release recovery requires schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("broker recovery not published"))?;
        let owner = record.ownership_id.clone();
        let binding = j
            .broker_bindings
            .iter()
            .find(|b| {
                b.ownership_id == owner
                    && b.helper_lease == release.helper_lease
                    && b.broker_lease.as_ref() == Some(&release.broker_lease)
            })
            .ok_or_else(|| anyhow::anyhow!("release lacks original broker mapping"))?;
        ensure!(j.ownership.iter().any(|o|o.ownership_id==owner && matches!(&o.ownership,NativeOwnership::Acquired {prepared,lease} if lease==&release.helper_lease && prepared.intent==release.helper_intent)),"release changed original helper intent");
        if let Some(old) = &record.broker_release {
            ensure!(
                old.helper_lease == release.helper_lease
                    && old.broker_lease == release.broker_lease
                    && old.helper_intent == release.helper_intent,
                "original broker release receipt changed"
            );
            return Ok(());
        }
        let settled = original_desktop_scope_settled(j, &owner, &binding.slot);
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            unreachable!()
        };
        let record = j
            .integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .unwrap();
        record.broker_release = Some(release);
        if settled {
            record.broker=Disposition::Complete {receipt:"original helper AwaitingExternalReset proves its prior original broker release barrier".into()};
        }
        let broker = record.broker.clone();
        self.save_journal()?;
        if let StoredRun::NativeIntegration(report) = &mut self.report
            && let Some(evidence) = report
                .integrations
                .iter_mut()
                .find(|s| s.ownership_id == owner)
        {
            evidence.dispositions.broker = broker;
        }
        self.save_report()
    }
    pub(crate) fn finish_integration_recovery(
        &mut self,
        recovery_id: &str,
        local: Disposition,
        broker: Disposition,
        provider: Disposition,
    ) -> Result<()> {
        local.validate()?;
        broker.validate()?;
        provider.validate()?;
        let StoredJournal::NativeIntegration(j) = &mut self.journal else {
            bail!("integration recovery requires selected schema2 journal")
        };
        let record = j
            .integration_recoveries
            .iter_mut()
            .find(|r| r.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?;
        record.local = local;
        record.broker = broker;
        record.provider = provider;
        self.save_journal()
    }
    /// A query-only recovery may learn the lease of its exact original intent.
    /// This publishes knowledge only, never a command-capable PublishedResource.
    pub(crate) fn record_recovered_lease(&mut self, ownership_id: &str, lease: &str) -> Result<()> {
        theseus_native_protocol::v2::LeaseId::parse(lease)?;
        let journal = self.native_journal()?;
        let owner = journal
            .ownership
            .iter_mut()
            .find(|owner| owner.ownership_id == ownership_id)
            .ok_or_else(|| anyhow::anyhow!("recovered owner missing"))?;
        match &owner.ownership {
            NativeOwnership::Intent { prepared } => {
                owner.ownership = NativeOwnership::Acquired {
                    prepared: prepared.clone(),
                    lease: lease.to_owned(),
                }
            }
            NativeOwnership::Acquired {
                lease: original, ..
            } => ensure!(original == lease, "recovered lease differs from original"),
            _ => bail!("recovered lease lacks original intent"),
        }
        self.save_journal()
    }
    /// A fresh query proved the original lease Held. Republish only that same
    /// release identity; no new command/Acquire capability can be constructed.
    pub(crate) fn publish_recovery_release(
        &mut self,
        ownership_id: &str,
    ) -> Result<PublishedRelease> {
        let journal = self.native_journal()?;
        let owner = journal
            .ownership
            .iter()
            .find(|owner| owner.ownership_id == ownership_id)
            .ok_or_else(|| anyhow::anyhow!("recovery owner missing"))?;
        let NativeOwnership::Acquired { lease, .. } = &owner.ownership else {
            bail!("recovery release requires original lease")
        };
        let release = if let Some(original) = journal
            .releases
            .iter()
            .find(|release| release.ownership_id == ownership_id && &release.lease == lease)
        {
            ensure!(
                matches!(original.completion, Disposition::Unknown { .. }),
                "terminal release conflicts with Held query"
            );
            original.clone()
        } else {
            let release = NativeRelease {
                ownership_id: ownership_id.to_owned(),
                lease: lease.clone(),
                completion: Disposition::Unknown {
                    reason: "recovery release published; settlement unproved".into(),
                },
            };
            journal.releases.push(release.clone());
            release
        };
        self.save_journal()?;
        Ok(PublishedRelease { release })
    }
    pub(crate) fn record_recovery_attempt(&mut self, attempt: NativeRecoveryAttempt) -> Result<()> {
        let journal = self.native_journal()?;
        ensure!(
            !journal
                .recovery_attempts
                .iter()
                .any(|old| old.recovery_id == attempt.recovery_id),
            "recovery attempt already published"
        );
        journal.recovery_attempts.push(attempt);
        self.save_journal()
    }
    pub(crate) fn finish_recovery_attempt(
        &mut self,
        recovery_id: &str,
        local: Disposition,
        remote: Disposition,
    ) -> Result<()> {
        let attempt = self
            .native_journal()?
            .recovery_attempts
            .iter_mut()
            .find(|attempt| attempt.recovery_id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("recovery attempt missing"))?;
        attempt.local = local;
        attempt.remote = remote;
        self.save_journal()
    }
}
