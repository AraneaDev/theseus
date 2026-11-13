use super::*;
#[test]
fn shipping_loader_refuses_arbitrary_command_and_secret_profiles() {
    let raw=br#"{"kind":"theseus_native_launch","schema":1,"program":"/fixture","args":[],"os":"linux","target_id":"fixture","binding":"01010101010101010101010101010101","manifest_digest":"0202020202020202020202020202020202020202020202020202020202020202","workspace_marker":"03030303030303030303030303030303","provider_epoch":"0808080808080808080808080808080808080808080808080808080808080808"}"#;
    assert!(LaunchAuthority::decode(raw).is_err());
    assert!(!unencrypted_private_key(
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot-key\n"
    ));
    assert!(!unencrypted_private_key(
        b"-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n"
    ));
}
#[test]
fn missing_changed_duplicate_or_truncated_effective_options_refuse() {
    let mut command = NativeCommand::new("/inert-ssh");
    command.args(["-o", "BatchMode=yes", "-o", "IdentityAgent=none"]);
    assert!(verify_effective_options(&command, "batchmode yes\nidentityagent none\n").is_ok());
    for output in [
        "batchmode yes\n",
        "batchmode no\nidentityagent none\n",
        "batchmode yes\nbatchmode yes\nidentityagent none\n",
        "batchmode yes\nidentityagent non",
    ] {
        assert!(verify_effective_options(&command, output).is_err())
    }
}
