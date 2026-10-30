//! Bounded inspection shared by cleanup and deliberate container transitions.
use super::*;

pub(super) struct OwnedState {
    pub(super) container_id: String,
    pub(super) labels: BTreeMap<String, String>,
    pub(super) running: bool,
}
impl OwnedState {
    fn verify(&self, resource: &OwnedResource) -> Result<()> {
        ensure!(
            self.container_id == resource.container_id,
            "container ID mismatch"
        );
        for (key, expected) in &resource.labels {
            ensure!(
                self.labels.get(key) == Some(expected),
                "container ownership label mismatch: {key}"
            );
        }
        Ok(())
    }
}

impl Docker {
    pub(super) fn inspect_owned(
        &self,
        resource: &OwnedResource,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Option<OwnedState>> {
        self.inspect_owned_until(
            resource,
            deadline.min(Instant::now() + Duration::from_secs(1)),
            cancel,
        )
    }

    fn inspect_owned_until(
        &self,
        resource: &OwnedResource,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Option<OwnedState>> {
        Self::validate_resource(resource)?;
        ensure!(!cancel.is_cancelled(), "ownership inspection cancelled");
        ensure!(
            Instant::now() < deadline,
            "ownership inspection deadline exceeded"
        );
        let output = process::run(
            self.command().args([
                "container",
                "inspect",
                "--format",
                "{{json .}}",
                &resource.container_id,
            ]),
            deadline.saturating_duration_since(Instant::now()),
            cancel,
            None,
            1_048_576,
            || Ok(()),
        )?;
        ensure!(
            !output.timed_out && !output.cancelled && output.error.is_none(),
            "ownership inspection interrupted: {:?}",
            output.error
        );
        if output.status != Some(0) {
            let error = String::from_utf8_lossy(&output.stderr.bytes)
                .trim()
                .to_ascii_lowercase();
            let id = &resource.container_id;
            if output.status == Some(1)
                && !output.stderr.truncated
                && [
                    format!("error: no such object: {id}"),
                    format!("error response from daemon: no such container: {id}"),
                ]
                .contains(&error)
            {
                return Ok(None);
            }
            bail!("ownership inspection failed: {error}");
        }
        ensure!(!output.stdout.truncated, "ownership metadata exceeds limit");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        let state = OwnedState {
            container_id: value["Id"].as_str().context("missing container ID")?.into(),
            labels: serde_json::from_value(value["Config"]["Labels"].clone())
                .context("missing container labels")?,
            running: value["State"]["Running"]
                .as_bool()
                .context("missing container running state")?,
        };
        state.verify(resource)?;
        Ok(Some(state))
    }

    pub(super) fn remove_owned_until(
        &mut self,
        resource: &OwnedResource,
        deadline: Instant,
    ) -> Result<()> {
        let cancel = CancellationToken::default();
        if self
            .inspect_owned_until(resource, deadline, &cancel)?
            .is_some()
        {
            self.owned_transition(&["rm", "-f", &resource.container_id], deadline, &cancel)?;
        }
        ensure!(
            self.inspect_owned_until(resource, deadline, &cancel)?
                .is_none(),
            "owned container remains after removal"
        );
        Ok(())
    }

    pub(super) fn verified_exists(&self, resource: &OwnedResource) -> Result<bool> {
        Ok(self
            .inspect_owned_until(
                resource,
                Instant::now() + Duration::from_secs(30),
                &CancellationToken::default(),
            )?
            .is_some())
    }

    pub(super) fn owned_transition(
        &self,
        args: &[&str],
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<()> {
        ensure!(!cancel.is_cancelled(), "container transition cancelled");
        ensure!(
            Instant::now() < deadline,
            "container transition deadline exceeded"
        );
        let output = process::run(
            self.command().args(args),
            operation_budget(deadline),
            cancel,
            None,
            512,
            || Ok(()),
        )?;
        ensure!(
            !output.cancelled && !output.timed_out && output.error.is_none(),
            "container transition interrupted"
        );
        ensure!(
            output.status == Some(0),
            "container transition failed (status {:?})",
            output.status
        );
        ensure!(
            !output.stdout.truncated && !output.stderr.truncated,
            "container transition output exceeds limit"
        );
        Ok(())
    }
}

pub(super) fn operation_budget(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(1))
}
