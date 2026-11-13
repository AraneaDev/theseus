use super::*;
impl Worker {
    pub(super) fn begin_desktop(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        if self.active.is_some()
            || self.file.is_some()
            || self.service_action.is_some()
            || self.desktop_tickets.len() >= 2
        {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        let lease = match &request {
            v2::Request::BrokerBind { original } | v2::Request::QueryBrokerBind { original } => {
                v2::LeaseId::parse(&original.helper_lease)
            }
            v2::Request::DesktopAction { request } => v2::LeaseId::parse(&request.helper_lease),
            v2::Request::InspectDesktop { lease, .. } | v2::Request::Release { lease } => {
                Ok(*lease)
            }
            v2::Request::QueryDesktop { identity }
            | v2::Request::CancelDesktop { identity }
            | v2::Request::ProbeDesktopReadiness { identity, .. } => {
                v2::LeaseId::parse(&identity.helper_lease)
            }
            _ => {
                self.refuse(ticket, v2::RefusalCode::InvalidRequest);
                return Ok(());
            }
        }
        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
        let original_control = matches!(
            request,
            v2::Request::QueryBrokerBind { .. }
                | v2::Request::QueryDesktop { .. }
                | v2::Request::CancelDesktop { .. }
        );
        if original_control {
            if !self
                .lease
                .as_ref()
                .is_some_and(|(_, actual, _)| *actual == lease)
            {
                self.refuse(ticket, v2::RefusalCode::WrongLease);
                return Ok(());
            }
        } else if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(desktop) = &mut self.desktop else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match desktop.start(self.store.as_mut().unwrap(), lease, request.clone()) {
            Ok(Some(reply)) => self.respond(ticket, reply, None),
            Ok(None) => self.desktop_tickets.push_back((request, ticket)),
            Err(error) => {
                desktop.fail(error);
                self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            }
        }
        Ok(())
    }
    pub(super) fn advance_desktop(&mut self) -> Result<(), WorkerPrimary> {
        if self.stopping {
            if let Some(desktop) = &mut self.desktop {
                desktop.request_stop();
            }
            self.desktop_tickets.clear();
            return Ok(());
        }
        let result = self
            .desktop
            .as_mut()
            .unwrap()
            .poll(self.store.as_mut().unwrap());
        match result {
            Ok(Some((original, reply))) => {
                let position = self
                    .desktop_tickets
                    .iter()
                    .position(|(request, _)| request == &original)
                    .ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "original desktop response ticket missing",
                        ))
                    })?;
                let (_, ticket) = self.desktop_tickets.remove(position).unwrap();
                if let v2::Reply::DesktopState { identity, state } = &reply {
                    self.publish(WorkerEvent::CacheUpdate(CachePatch::DesktopState {
                        identity: *identity.clone(),
                        state: *state.clone(),
                    }));
                }
                if let (
                    v2::Request::Release { lease },
                    v2::Reply::Lease {
                        state: v2::LeaseView::AwaitingExternalReset { .. },
                        ..
                    },
                ) = (&original, &reply)
                {
                    self.release(*lease, ticket)?;
                } else {
                    self.response = Some((ticket, reply, None));
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.desktop.as_mut().unwrap().fail(error);
                if let Some((_, ticket)) = self.desktop_tickets.pop_front() {
                    self.refuse(ticket, v2::RefusalCode::OwnershipPending);
                }
            }
        }
        Ok(())
    }
}
