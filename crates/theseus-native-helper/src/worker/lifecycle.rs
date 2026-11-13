use super::*;
impl Worker {
    pub(super) fn initialize(&mut self, inv: Invocation) -> Result<(), WorkerPrimary> {
        if self.invocation.is_some() {
            return Err(WorkerPrimary::Io(io::Error::other(
                "duplicate initialization",
            )));
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = &inv.fixture {
            control.init_reached();
        }
        let ledger =
            HostDir::open(&inv.ledger_root, RootPolicy::LeafNoFollow).map_err(WorkerPrimary::Io)?;
        let workspace = HostDir::open(&inv.workspace_root, RootPolicy::LeafNoFollow)
            .map_err(WorkerPrimary::Io)?;
        let expected = theseus_native_helper_store::StoreBinding {
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
            workspace_marker: inv.expected.workspace_marker,
            os: inv.expected.os,
        };
        if expected.os != crate::invocation::host_os() {
            return Err(WorkerPrimary::Io(io::Error::other("wrong host OS")));
        }
        let store = match inv.profile {
            v2::HelperProfile::Integration { .. } => {
                LedgerStore::open_profile(&ledger, &workspace, expected, inv.profile)
            }
            v2::HelperProfile::Fixture => LedgerStore::open(&ledger, &workspace, expected),
            v2::HelperProfile::Native { provider_epoch } => {
                LedgerStore::open_native(&ledger, &workspace, expected, provider_epoch)
            }
        }
        .map_err(WorkerPrimary::Store)?;
        self.native_authority_failure = store.restart_view().native_authority_failure();
        let helper_principal =
            match theseus_host::principal::current_principal().map_err(WorkerPrimary::Io)? {
                theseus_host::principal::CurrentPrincipal::Posix {
                    effective_uid,
                    effective_gid,
                } => v2::Principal::Posix {
                    effective_uid,
                    effective_gid,
                },
                theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
                    v2::Principal::Windows {
                        token_user_sid: v2::SidText::new(&token_user_sid).map_err(|_| {
                            WorkerPrimary::Io(io::Error::other("invalid native principal"))
                        })?,
                    }
                }
            };
        let mut operations = Vec::new();
        for record in store.records() {
            match record.entry() {
                Entry::LeaseReserved { intent, lease } => {
                    self.lease = Some((
                        *intent,
                        *lease,
                        store
                            .restart_view()
                            .query_lease(*lease)
                            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid lease")))?,
                    ))
                }
                Entry::OperationAccepted {
                    lease,
                    operation,
                    request_digest,
                    operation_kind,
                } => operations.push(RecoveredOperation {
                    lease: *lease,
                    operation: *operation,
                    request_digest: *request_digest,
                    kind: *operation_kind,
                    view: store
                        .restart_view()
                        .query_operation(*lease, *operation)
                        .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid operation")))?,
                }),
                _ => {}
            }
        }
        let description = v2::Reply::Description {
            binding: inv.expected.binding,
            manifest_digest: inv.expected.manifest_digest,
            workspace_marker: inv.expected.workspace_marker,
            os: crate::invocation::host_os(),
            helper_principal,
            programs: v2::ProgramList::new(inv.catalog.iter().map(|e| e.id.clone()).collect())
                .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid catalog")))?,
        };
        let description = match (inv.profile, description) {
            (
                v2::HelperProfile::Native { provider_epoch }
                | v2::HelperProfile::Integration { provider_epoch, .. },
                v2::Reply::Description {
                    binding,
                    manifest_digest,
                    workspace_marker,
                    os,
                    helper_principal,
                    programs,
                },
            ) => v2::Reply::NativeDescription {
                description: Box::new(v2::NativeDescription {
                    binding,
                    manifest_digest,
                    workspace_marker,
                    os,
                    helper_principal,
                    programs,
                    capabilities: v2::NativeCapabilities,
                    provider_epoch,
                    artifact_commitment: inv.artifact_commitment.ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "native catalog artifact commitment missing",
                        ))
                    })?,
                    build: v2::Provenance::Unavailable {
                        reason: "build provenance unavailable".into(),
                    },
                    os_build: v2::Provenance::Unavailable {
                        reason: "OS build provenance unavailable".into(),
                    },
                }),
            },
            (_, description) => description,
        };
        let description = if let v2::HelperProfile::Integration {
            core_manifest_digest,
            integration_manifest_digest,
            ..
        } = inv.profile
        {
            let config = inv.integration.as_ref().ok_or_else(|| {
                WorkerPrimary::Io(io::Error::other("integration context missing"))
            })?;
            if config.manifest.desktop.is_some() {
                let declared = v2::broker::declared_desktop(store.records())
                    .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
                if declared
                    != config
                        .manifest
                        .desktop
                        .as_ref()
                        .zip(config.context.desktop_broker_header.as_ref())
                {
                    return Err(WorkerPrimary::Io(io::Error::other(
                        "desktop ceiling differs from original helper declaration",
                    )));
                }
            }
            let context_digest = config
                .context
                .digest()
                .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
            match store.records()[0].entry() {
                Entry::IntegrationHeader {
                    context_digest: stored,
                    ownership_id,
                    artifact_commitment,
                    ..
                } if *stored == context_digest
                    && ownership_id == &config.context.ownership_id
                    && Some(*artifact_commitment) == inv.artifact_commitment => {}
                _ => {
                    return Err(WorkerPrimary::Io(io::Error::other(
                        "integration runtime context differs from original header",
                    )));
                }
            }
            let v2::Reply::NativeDescription {
                description: native,
            } = description
            else {
                return Err(WorkerPrimary::Io(io::Error::other(
                    "integration native description missing",
                )));
            };
            if config.manifest.desktop.is_some() {
                let mut desktop =
                    crate::integration::desktop::DesktopRuntime::new(config, &inv.ledger_root)
                        .map_err(WorkerPrimary::Io)?;
                desktop
                    .retain_provider_qualification(&store, &config.context)
                    .map_err(WorkerPrimary::Io)?;
                self.desktop = Some(desktop);
            }
            if config.manifest.service_authority.is_some() {
                self.service = Some(crate::integration::service::ServiceRuntime::new(
                    config.as_ref().clone(),
                ));
            }
            v2::Reply::IntegrationDescription {
                description: Box::new(v2::IntegrationDescription {
                    manifest: Box::new(config.manifest.clone()),
                    namespace_receipt_digest: config
                        .context
                        .authority_receipt_digest()
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
                    native: *native,
                    core_manifest_digest,
                    integration_manifest_digest,
                    context_digest,
                    ownership_id: config.context.ownership_id.clone(),
                    capabilities: v2::IntegrationCapabilities::for_manifest(&config.manifest)
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
                }),
            }
        } else {
            description
        };
        self.store = Some(store);
        self.workspace = Some(workspace);
        self.invocation = Some(inv);
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Initialized {
            description,
            lease: self.lease.clone(),
            operations,
        }));
        Ok(())
    }
    pub(super) fn acquire(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::Acquire {
            intent,
            binding,
            manifest_digest,
        } = request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let Some(inv) = &self.invocation else {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        };
        if inv.expected.binding != binding {
            self.refuse(ticket, v2::RefusalCode::WrongBinding);
            return Ok(());
        }
        if inv.expected.manifest_digest != manifest_digest {
            self.refuse(ticket, v2::RefusalCode::WrongManifest);
            return Ok(());
        }
        if let Some((existing, lease, view)) = &self.lease {
            let reply = if *existing == intent {
                v2::Reply::Acquired {
                    intent,
                    binding,
                    manifest_digest,
                    state: v2::AcquireView::Lease {
                        lease: *lease,
                        state: view.clone(),
                    },
                }
            } else {
                v2::Reply::Refused {
                    code: v2::RefusalCode::Busy,
                    inspect_failure: None,
                }
            };
            self.respond(ticket, reply, None);
            return Ok(());
        }
        let mut bytes = [0; 16];
        theseus_host::random_bytes(&mut bytes).map_err(WorkerPrimary::Io)?;
        let lease = v2::LeaseId::from_bytes(bytes);
        let view = v2::LeaseView::Held { intent };
        let _committed = self.append(Entry::LeaseReserved { intent, lease })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_acquire_committed();
        }
        self.lease = Some((intent, lease, view.clone()));
        self.response = Some((
            ticket,
            v2::Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state: v2::AcquireView::Lease {
                    lease,
                    state: view.clone(),
                },
            },
            None,
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease,
            view,
        }));
        Ok(())
    }
    fn native_mapping(&mut self) -> Option<v2::PrefixAuthorityFailure> {
        if let Some(reason) = self.native_authority_failure {
            return Some(reason);
        }
        let inv = self.invocation.as_ref()?;
        if inv.profile == v2::HelperProfile::Fixture {
            return None;
        }
        let result =
            HostDir::open(&inv.workspace_root, RootPolicy::LeafNoFollow).and_then(|current| {
                self.workspace
                    .as_ref()
                    .unwrap()
                    .same_directory_object(&current)
            });
        match result {
            Ok(true) => None,
            Ok(false) => Some(v2::PrefixAuthorityFailure::Replaced),
            Err(error) => {
                // Generic root/identity errors do not prove completion of every
                // subordinate native query. Retain the original source and root
                // through conservative incomplete shutdown, never Clean.
                self.prefix_mapping_error = Some(error);
                self.stopping = true;
                Some(v2::PrefixAuthorityFailure::MappingUnproved)
            }
        }
    }
    pub(super) fn check_native_mapping(
        &mut self,
        lease: v2::LeaseId,
    ) -> Result<Option<v2::PrefixAuthorityFailure>, WorkerPrimary> {
        let Some(reason) = self.native_mapping() else {
            return Ok(None);
        };
        if self.native_authority_failure.is_none() {
            // Revoke in memory before publication; uncertain persistence cannot grant effects.
            self.native_authority_failure = Some(reason);
            self.append(Entry::NativeAuthorityRevoked { lease, reason })?;
        }
        Ok(Some(reason))
    }
    pub(super) fn held_refusal(&self, lease: v2::LeaseId) -> Option<v2::RefusalCode> {
        match &self.lease {
            Some((_, actual, _)) if *actual != lease => Some(v2::RefusalCode::WrongLease),
            Some((_, _, v2::LeaseView::Held { .. })) => None,
            Some((_, _, v2::LeaseView::Quarantined { .. })) => {
                Some(v2::RefusalCode::LeaseQuarantined)
            }
            Some(_) => Some(v2::RefusalCode::AwaitingExternalReset),
            None => Some(v2::RefusalCode::WrongLease),
        }
    }
    pub(super) fn release(
        &mut self,
        lease: v2::LeaseId,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        if self.desktop.is_some() {
            let desktop_view =
                v2::broker::BrokerLedgerView::replay(self.store.as_ref().unwrap().records())
                    .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
            if desktop_view.binding_original().is_some() && !desktop_view.helper_released() {
                return self.begin_desktop(v2::Request::Release { lease }, ticket);
            }
        }
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        let Some((intent, actual, old)) = self.lease.clone() else {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        };
        if actual != lease {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        }
        if !matches!(old, v2::LeaseView::Held { .. }) {
            self.respond(ticket, v2::Reply::Lease { lease, state: old }, None);
            return Ok(());
        }
        if let Some(service) = &mut self.service
            && let Err(error) = service.cleanup_readiness()
        {
            service.errors.push(error);
            service.uncertain = true;
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        self.append(Entry::LeaseClosed {
            lease,
            disposition: Disposition::AwaitingExternalReset,
        })?;
        let view = v2::LeaseView::AwaitingExternalReset { intent };
        self.lease = Some((intent, lease, view.clone()));
        self.response = Some((
            ticket,
            v2::Reply::Lease {
                lease,
                state: view.clone(),
            },
            None,
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease,
            view,
        }));
        Ok(())
    }
}
