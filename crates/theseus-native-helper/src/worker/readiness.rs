use super::*;
impl Worker {
    pub(super) fn probe_readiness(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::ProbeReadiness {
            lease, deadline_ms, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let lease = *lease;
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(service) = &mut self.service else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match service.probe(
            &request,
            Instant::now() + Duration::from_millis(u64::from(*deadline_ms)),
            &self.ports.stop,
        ) {
            Ok(observation) => self.respond(
                ticket,
                v2::Reply::ReadinessObservation {
                    lease,
                    observation: Box::new(observation),
                },
                None,
            ),
            Err(error) => {
                service.errors.push(error);
                self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            }
        }
        Ok(())
    }
}
