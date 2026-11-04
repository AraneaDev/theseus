use super::invalid;
use crate::desktop::owned::{CommittedReadinessProbe, ExecutableBinding};
use std::io;
pub(super) fn validate_static_bootstrap(
    arguments: &[String],
    ready: &str,
    close: &str,
) -> io::Result<()> {
    if ready == close {
        return Err(invalid("separate committed native channels required"));
    }
    for key in ["--theseus-instance", "--theseus-activation-nonce"] {
        if arguments
            .iter()
            .any(|arg| arg == key || arg.starts_with(&format!("{key}=")))
        {
            return Err(invalid(
                "per-action native identity belongs in the original-object authenticated challenge, not installed argv",
            ));
        }
    }
    for (key, value) in [
        ("--theseus-bootstrap", "theseus_fixture_ready_bootstrap_v1"),
        ("--theseus-ready", ready),
        ("--theseus-close", close),
    ] {
        let expected = format!("{key}={value}");
        if arguments
            .iter()
            .filter(|arg| arg.as_str() == key || arg.starts_with(&format!("{key}=")))
            .count()
            != 1
            || !arguments.contains(&expected)
        {
            return Err(invalid(
                "native argv lacks exact closed bootstrap/channel commitment",
            ));
        }
    }
    Ok(())
}
pub(super) fn pin_poll_deadline(
    original: &mut Option<std::time::Instant>,
    deadline: std::time::Instant,
) -> io::Result<()> {
    if deadline > std::time::Instant::now() + std::time::Duration::from_secs(30)
        || original.is_some_and(|old| old != deadline)
    {
        return Err(invalid(
            "native probe must preserve its original bounded semantic deadline",
        ));
    }
    *original = Some(deadline);
    Ok(())
}
pub(super) fn validate_fresh_probe(
    original: &ExecutableBinding,
    next: &CommittedReadinessProbe,
    used: &[[u8; 16]],
) -> io::Result<()> {
    crate::services::readiness::encode_challenge(&next.challenge)?;
    let c = &next.challenge;
    let old = &original.ready;
    if c.protocol != old.protocol
        || c.lease != old.lease
        || c.slot != old.slot
        || c.resource_binding_digest != old.resource_binding_digest
        || c.start_or_activation_operation != old.start_or_activation_operation
        || c.action_digest != old.action_digest
        || c.expectation_digest == [0; 32]
        || c.nonce == [0; 16]
        || c.nonce == original.close.nonce
        || used.contains(&c.nonce)
        || used.len() >= 64
        || next.expectation.version_literal.is_empty()
        || next.expectation.version_literal.len() > 256
        || next.expectation.version_literal.as_bytes().contains(&0)
    {
        return Err(invalid(
            "fresh native probe must preserve original activation and use a new bounded committed nonce",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod polling_tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn argv() -> Vec<String> {
        vec![
            "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1".into(),
            "--theseus-ready=/private/ready".into(),
            "--theseus-close=/private/close".into(),
            "--data=/private/data".into(),
        ]
    }
    #[test]
    fn native_polling_static_bootstrap_commits_channels_without_action_argv() {
        validate_static_bootstrap(&argv(), "/private/ready", "/private/close").unwrap();
        for bad in [
            "--theseus-instance=action-digest",
            "--theseus-activation-nonce=11111111111111111111111111111111",
            "--theseus-ready=/foreign/ready",
            "--theseus-bootstrap=unknown",
        ] {
            let mut args = argv();
            args.push(bad.into());
            assert!(
                validate_static_bootstrap(&args, "/private/ready", "/private/close").is_err(),
                "{bad}"
            );
        }
        let mut missing = argv();
        missing.remove(0);
        assert!(validate_static_bootstrap(&missing, "/private/ready", "/private/close").is_err());
    }
    #[test]
    fn native_polling_keeps_one_original_semantic_deadline() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut original = None;
        pin_poll_deadline(&mut original, deadline).unwrap();
        pin_poll_deadline(&mut original, deadline).unwrap();
        assert!(pin_poll_deadline(&mut original, deadline + Duration::from_secs(1)).is_err());
        assert_eq!(original, Some(deadline));
        assert!(pin_poll_deadline(&mut None, Instant::now() + Duration::from_secs(31)).is_err());
    }
}
