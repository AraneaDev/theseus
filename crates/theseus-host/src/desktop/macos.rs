//! Exact retained macOS bundle admission and original manager-instance lifetime.
//!
//! The offline profile/progress types grant no launch authority. Only native CF
//! parsing can construct an AdmittedBundle; only an explicitly authorized Aqua
//! broker may call the separate LaunchServices adapter. Durable original broker
//! acceptance and exact profile/argument admission are caller prerequisites.
use std::io;
#[cfg(any(target_os = "macos", test))]
mod probe;
#[cfg(unix)]
mod retained;
#[cfg(any(target_os = "macos", test))]
mod retention;
#[cfg(unix)]
pub use retained::RetainedBundle;
#[cfg(target_os = "macos")]
mod metadata;
#[cfg(target_os = "macos")]
pub use metadata::{AdmittedBundle, native_metadata};
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub use native::{
    NativeActivation, NativeLifetime, NativeStartFailure, pump_once, validate_supplied_session,
};

#[derive(Clone, Debug)]
pub struct ArtifactCommitment {
    pub length: u64,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug)]
pub struct BundleProfile {
    /// Exactly one supplied root leaf; no bundle identifier lookup.
    pub app: String,
    pub bundle_identifier: String,
    pub executable: String,
    /// All additional committed launch arguments, excluding executable argv[0].
    /// NSOpenConfiguration inserts the app path; static bootstrap tokens stay exact.
    pub arguments: Vec<String>,
    pub info: ArtifactCommitment,
    pub executable_artifact: ArtifactCommitment,
}
/// Native parser output, or explicitly labelled offline fixture metadata.
#[derive(Clone, Debug)]
pub struct BundleMetadata {
    pub identifier: String,
    pub executable: String,
    pub package_type: String,
    pub extra_keys: Vec<String>,
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn leaf(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
impl BundleProfile {
    pub fn validate(&self) -> io::Result<()> {
        if !leaf(&self.app)
            || !self.app.ends_with(".app")
            || self.app.len() <= 4
            || !leaf(&self.executable)
            || self.bundle_identifier.len() > 255
            || !self.bundle_identifier.contains('.')
            || !self.bundle_identifier.split('.').all(|part| {
                !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
            || self.info.length == 0
            || self.info.length > 65536
            || self.executable_artifact.length == 0
            || self.executable_artifact.length > 64 * 1024 * 1024
            || self.info.sha256 == [0; 32]
            || self.executable_artifact.sha256 == [0; 32]
            || self.arguments.len() > 64
            || self
                .arguments
                .iter()
                .any(|arg| arg.len() > 1024 || arg.as_bytes().contains(&0))
            || self.arguments.iter().map(String::len).sum::<usize>() > 16384
        {
            return Err(invalid("unsupported exact macOS bundle profile"));
        }
        Ok(())
    }
    pub fn admit_metadata(&self, metadata: &BundleMetadata) -> io::Result<()> {
        self.validate()?;
        if metadata.identifier != self.bundle_identifier
            || metadata.executable != self.executable
            || metadata.package_type != "APPL"
            || !metadata.extra_keys.is_empty()
        {
            return Err(invalid(
                "bundle metadata differs from closed admitted profile",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationEffect {
    Pending,
    /// Launcher returned a manager instance, without fresh cooperative proof.
    Unavailable,
    Acknowledged,
    Unknown,
}
#[derive(Clone, Copy, Debug)]
pub enum CallbackResult {
    OriginalManagerInstance,
    Failed,
    Ambiguous,
}
/// Pure state projection; never an original native owner or cleanup authority.
pub struct ActivationProgress {
    effect: ActivationEffect,
    callback_pending: bool,
    original_instance: bool,
    authenticated: bool,
    ready: bool,
    terminated: bool,
}
impl ActivationProgress {
    pub fn pending() -> Self {
        Self {
            effect: ActivationEffect::Pending,
            callback_pending: true,
            original_instance: false,
            authenticated: false,
            ready: false,
            terminated: false,
        }
    }
    pub fn effect(&self) -> ActivationEffect {
        self.effect
    }
    pub fn callback(&mut self, result: CallbackResult) -> io::Result<()> {
        if !self.callback_pending {
            return Err(invalid("duplicate native completion"));
        }
        self.callback_pending = false;
        match result {
            CallbackResult::OriginalManagerInstance => {
                self.original_instance = true;
                self.effect = ActivationEffect::Unavailable;
            }
            CallbackResult::Failed => self.effect = ActivationEffect::Unavailable,
            CallbackResult::Ambiguous => self.effect = ActivationEffect::Unknown,
        }
        Ok(())
    }
    pub fn authenticate_instance(&mut self, matches_original: bool) -> io::Result<()> {
        if self.callback_pending
            || !self.original_instance
            || self.effect == ActivationEffect::Unknown
        {
            return Err(invalid(
                "no original manager instance available to authenticate",
            ));
        }
        if matches_original {
            self.authenticated = true;
            self.ready = true;
            self.effect = ActivationEffect::Acknowledged;
        } else {
            self.authenticated = false;
            self.effect = ActivationEffect::Unknown;
        }
        Ok(())
    }
    pub fn application_ready(&self) -> bool {
        self.authenticated && self.ready && !self.terminated
    }
    pub fn observe_application_ready(&mut self, ready: bool) {
        self.ready = ready && self.authenticated;
    }
    pub fn descendants_owned(&self) -> bool {
        false
    }
    /// A deadline/cancel is not a native callback or resource-retirement event.
    pub fn mark_deadline_or_cancel(&mut self) {}
    pub fn mark_native_uncertain(&mut self) {
        self.effect = ActivationEffect::Unknown;
        self.ready = false;
    }
    pub fn callback_pending(&self) -> bool {
        self.callback_pending
    }
    pub fn manager_terminated(&mut self) {
        self.terminated = true;
    }
    pub fn manager_instance_terminated(&self) -> bool {
        self.terminated
    }
}
#[cfg(test)]
mod tests;
