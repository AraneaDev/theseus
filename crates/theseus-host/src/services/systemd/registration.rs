//! Registration qualification, configuration admission and owner verification.
use super::sdk::invalid;
use super::{
    LoadedService, RegistrationOwner, SystemdDefinition, SystemdManager, validate_definition_source,
};
use std::{io, sync::atomic::AtomicBool, time::Instant};

impl SystemdManager {
    pub fn qualify_registration(
        &mut self,
        definition: &SystemdDefinition,
        install_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<RegistrationOwner> {
        if self.revoked {
            return Err(invalid("original systemd authority permanently revoked"));
        }
        definition.validate()?;
        if install_receipt.is_empty() || install_receipt.len() > 4096 {
            return Err(invalid("missing original accepted SUT install receipt"));
        }
        let observation = self.inspect(deadline, cancel)?;
        let Some(loaded) = observation.loaded else {
            return Err(invalid(
                "installed registration not loadable by the original manager",
            ));
        };
        if let Err(error) = self.check_definition(&loaded, definition) {
            self.revoked = true;
            return Err(error);
        }
        if !observation.definition_present || observation.unit_file_state.is_none() {
            return Err(invalid("registration definition is absent"));
        }
        let Some((file, fingerprint, definition_bytes)) = self.definition_file()? else {
            return Err(invalid("registration disappeared"));
        };
        if !matches!(
            observation.unit_file_state.as_deref(),
            Some("static" | "disabled")
        ) {
            self.revoked = true;
            return Err(invalid(
                "automatic activation or unsupported unit-file state",
            ));
        }
        if let Err(error) = validate_definition_source(&definition_bytes) {
            self.revoked = true;
            return Err(error);
        }
        Ok(RegistrationOwner {
            _file: file,
            prior_files: vec![],
            transition_receipts: vec![],
            fingerprint,
            definition_bytes,
            definition: definition.clone(),
            manager_instance: self.instance,
            install_receipt,
            revoked: false,
        })
    }
    /// Admit an explicit completed SUT definition transition on the retained owner.
    /// The old file owners remain held; neither a name lookup nor a restored
    /// definition can revive a previously revoked registration.
    pub fn admit_configuration(
        &mut self,
        owner: &mut RegistrationOwner,
        definition: &SystemdDefinition,
        expected_bytes: &[u8],
        sut_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.revoked || owner.revoked || owner.manager_instance != self.instance {
            owner.revoked = true;
            self.revoked = true;
            return Err(invalid("original registration permanently revoked"));
        }
        if self.pending.is_some()
            || owner.transition_receipts.len() >= 128
            || owner.install_receipt == sut_receipt
            || owner.transition_receipts.contains(&sut_receipt)
            || sut_receipt.is_empty()
            || sut_receipt.len() > 4096
            || sut_receipt.chars().any(char::is_control)
        {
            return Err(invalid("original systemd SUT transition unavailable"));
        }
        let result = (|| {
            definition.validate()?;
            self.verify_context(deadline, cancel)?;
            let observation = self.inspect(deadline, cancel)?;
            let loaded = observation
                .loaded
                .as_ref()
                .ok_or_else(|| invalid("SUT transition is not loaded"))?;
            self.check_definition(loaded, definition)?;
            let (file, fingerprint, bytes) = self
                .definition_file()?
                .ok_or_else(|| invalid("SUT transition definition missing"))?;
            if bytes != expected_bytes
                || observation.definition_bytes.as_ref() != Some(&bytes)
                || !matches!(
                    observation.unit_file_state.as_deref(),
                    Some("static" | "disabled")
                )
            {
                return Err(invalid(
                    "SUT transition differs from exact admitted definition",
                ));
            }
            validate_definition_source(&bytes)?;
            Ok((file, fingerprint, bytes))
        })();
        match result {
            Ok((file, fingerprint, bytes)) => {
                owner.record_transition(file, fingerprint, bytes, definition, sut_receipt)
            }
            Err(error) => {
                owner.revoked = true;
                self.revoked = true;
                Err(error)
            }
        }
    }
    pub(super) fn check_definition(
        &self,
        loaded: &LoadedService,
        expected: &SystemdDefinition,
    ) -> io::Result<()> {
        self.check_origin(loaded)?;
        if loaded.load_state != "loaded"
            || loaded.fragment_path.is_empty()
            || loaded.need_daemon_reload
            || loaded.service_type != "simple"
            || loaded.restart != "no"
            || loaded.definition.as_ref() != Some(expected)
        {
            return Err(invalid(
                "loaded configuration does not match qualified definition",
            ));
        }
        Ok(())
    }
    pub fn verify_registration(
        &mut self,
        owner: &mut RegistrationOwner,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.verify_owner(owner, deadline, cancel)
    }
    pub(super) fn verify_owner(
        &mut self,
        owner: &mut RegistrationOwner,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.revoked || owner.revoked || owner.manager_instance != self.instance {
            owner.revoked = true;
            self.revoked = true;
            return Err(invalid("registration owner not original to manager"));
        }
        let result = (|| {
            let observation = self.inspect(deadline, cancel)?;
            let Some(loaded) = observation.loaded else {
                return Err(invalid("original registration no longer loaded"));
            };
            self.check_definition(&loaded, &owner.definition)?;
            let Some((_, current, bytes)) = self.definition_file()? else {
                return Err(invalid("original definition missing"));
            };
            if current != owner.fingerprint || bytes != owner.definition_bytes {
                return Err(invalid("original definition replaced or modified"));
            }
            Ok(())
        })();
        if result.is_err() {
            owner.revoked = true;
            self.revoked = true;
        }
        result
    }
}
