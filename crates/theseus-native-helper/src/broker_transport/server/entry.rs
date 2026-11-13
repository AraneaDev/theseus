use super::*;
impl BrokerServer {
    pub(super) fn revalidate_session(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(factory) = &self.native_factory
            && let Err(error) = factory.revalidate()
        {
            let _ = self.runtime.observe_session(None);
            return Err(error);
        }
        let measured = measure_local_identity()?;
        if measured != self.measurement {
            let _ = self.runtime.observe_session(None);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original broker process/session changed",
            ));
        }
        Ok(())
    }
    fn quarantine_entry(&mut self) -> io::Result<()> {
        self.definition_revoked = true;
        self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
        Ok(())
    }
    pub(super) fn retire_entry_proof(&mut self) -> io::Result<()> {
        if self.definition.is_none() {
            return Ok(());
        }
        if self.retired_definitions.len() >= v2::MAX_OPERATIONS {
            self.definition_revoked = true;
            self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
            return Err(io::Error::other("entry tombstone capacity"));
        }
        let result = self
            .roots
            .revalidate()
            .and_then(|()| self.definition.as_ref().unwrap().revalidate())
            .and_then(|()| {
                self.definition.as_mut().unwrap().retire_owned_entry(
                    #[cfg(windows)]
                    &mut self.apartment,
                )
            });
        if let Err(error) = result {
            self.definition_revoked = true;
            self.runtime.commit(BrokerEvent::EntryProofQuarantined {})?;
            let operations = BrokerLedgerView::replay(self.runtime.store().records())
                .map_err(io::Error::other)?
                .requests()
                .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
                .last()
                .map(|r| r.helper_operation.clone());
            if let Some(operation) = operations {
                self.runtime
                    .commit(BrokerEvent::Quarantined { operation })?;
            }
            self.definition_revoked = true;
            return Err(error);
        }
        self.retired_definitions
            .push(self.definition.take().unwrap());
        self.definition_used = false;
        Ok(())
    }
    pub(super) fn observe_owned_sut(
        &mut self,
        sut: Option<&DesktopSutRunWitness>,
    ) -> io::Result<()> {
        if self.config.manifest.desktop_owned_entry.is_none() {
            return Err(io::Error::other("owned entry inspection not declared"));
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        if view.entry_blocked()
            || view.entry_preparations().len() != self.prepared_entries.len()
            || (view.requests().next().is_some() && self.prepared_entries.is_empty())
        {
            return Err(io::Error::other(
                "entry preparation pending/quarantined/original continuity lost",
            ));
        }
        if sut == self.observed_sut.as_ref() {
            return Ok(());
        }
        let sut = sut.ok_or_else(|| io::Error::other("original SUT witness disappeared"))?;
        sut.validate().map_err(io::Error::other)?;
        let prepared = self
            .prepared_entries
            .last()
            .ok_or_else(|| io::Error::other("original SUT preparation continuity missing"))?;
        if sut.lease != prepared.helper_lease
            || sut.operation != prepared.operation
            || sut.request_digest != prepared.request_digest
            || sut.program != prepared.program
        {
            return Err(io::Error::other(
                "completed SUT differs from original prepared Run",
            ));
        }
        if self.definition.is_some() || self.unconsumed_launch.is_some() {
            return Err(io::Error::other("prior original proof remains retained"));
        }
        self.generation = None;
        self.observed_sut = Some(sut.clone());
        Ok(())
    }
    pub(super) fn prepare_owned_entry(&mut self, preparation: &EntryPreparation) -> io::Result<()> {
        preparation.validate().map_err(io::Error::other)?;
        if self.config.manifest.desktop_owned_entry.is_none()
            || self.definition_revoked
            || self.unconsumed_launch.is_some()
            || self.pending.is_some()
        {
            return Err(io::Error::other("entry retirement not available"));
        }
        let view =
            BrokerLedgerView::replay(self.runtime.store().records()).map_err(io::Error::other)?;
        if view.entry_quarantined() {
            return Err(io::Error::other("entry proof quarantined"));
        }
        if let Some((old, settled)) = view
            .entry_preparations()
            .iter()
            .find(|(p, _)| p.operation == preparation.operation)
        {
            if old == preparation && *settled && self.prepared_entries.contains(preparation) {
                return Ok(());
            }
            return Err(io::Error::other(
                "original entry preparation is pending/lost/changed",
            ));
        }
        let lease = v2::LeaseId::parse(&preparation.broker_lease).map_err(io::Error::other)?;
        if !matches!(
            self.runtime.store().restart_view().query_lease(lease),
            Ok(v2::LeaseView::Held { .. })
        ) {
            return Err(io::Error::other("entry preparation wrong broker lease"));
        }
        let observation = self
            .last_entry_observation
            .as_ref()
            .ok_or_else(|| io::Error::other("original entry inspection missing"))?;
        if !entry_predecessor_matches(
            preparation,
            observation,
            self.observed_sut.as_ref(),
            self.prepared_entries.last(),
            &self
                .config
                .header
                .digest()
                .map_err(io::Error::other)?
                .to_hex(),
        ) {
            return Err(io::Error::other(
                "original entry preparation chronology changed",
            ));
        }
        if let Some(original) = view
            .requests()
            .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
            .last()
        {
            let identity = original.identity().map_err(io::Error::other)?;
            if !view
                .query(original)
                .map_err(io::Error::other)?
                .original_retired()
                || self.runtime.retirement_continuity()?.as_ref() != Some(&identity)
            {
                return Err(io::Error::other("original activated owner not retired"));
            }
        }
        let mapping = (|| {
            self.roots.revalidate()?;
            if let Some(definition) = &self.definition {
                definition.revalidate()?;
            } else if observation.entry_present {
                // Only the latest inspected original, never obsolete historical images.
                self.retired_definitions
                    .last()
                    .ok_or_else(|| io::Error::other("original entry tombstone missing"))?
                    .revalidate_original_files()?;
            } else {
                let selected = super::super::generation::inspection(
                    &self.config,
                    &view,
                    preparation.generation.as_ref(),
                    self.runtime.retirement_continuity()?.as_ref(),
                )?;
                let current = launch::inspect(&self.config, &self.roots, &selected)?;
                if current.entry_present || current.executable_digest.is_some() {
                    return Err(io::Error::other("original vacant entry mapping changed"));
                }
            }
            Ok(())
        })();
        if let Err(error) = mapping {
            self.quarantine_entry()?;
            return Err(error);
        }
        self.runtime.commit(BrokerEvent::EntryPreparationAccepted {
            preparation: preparation.clone(),
        })?;
        self.retire_entry_proof()?;
        self.runtime.commit(BrokerEvent::EntryPreparationSettled {
            operation: preparation.operation.clone(),
        })?;
        self.prepared_entries.push(preparation.clone());
        Ok(())
    }
}
pub(super) fn entry_predecessor_matches(
    p: &EntryPreparation,
    observation: &DesktopObservation,
    observed_sut: Option<&DesktopSutRunWitness>,
    previous: Option<&EntryPreparation>,
    header_digest: &str,
) -> bool {
    if !observation.complete
        || p.slot != observation.slot
        || p.header_digest != header_digest
        || p.binding_digest != observation.binding_digest
        || p.generation != observation.generation
        || p.prior_sut.as_ref() != observed_sut
        || observation.entry_present && !observation.entry_valid
        || previous.is_some_and(|old| old.prior_sut == p.prior_sut)
    {
        return false;
    }
    match p.program.as_str() {
        "install_base" => {
            !observation.entry_present
                && observed_sut.is_none_or(|sut| sut.program.as_str() == "uninstall")
        }
        "install_target" | "upgrade" | "reinstall" => {
            if observation.entry_present {
                observed_sut.is_some_and(|sut| sut.program.as_str() == "configure")
            } else {
                observed_sut.is_none_or(|sut| sut.program.as_str() == "uninstall")
            }
        }
        "configure" | "uninstall" => {
            observation.entry_present
                && observed_sut.is_some_and(|sut| sut.program.as_str() != "uninstall")
        }
        _ => false,
    }
}
