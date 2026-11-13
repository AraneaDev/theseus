//! Exact supplied GIO broker environment validation.
use super::{absolute, invalid};
use crate::desktop::session::{BoundSession, SuppliedDisplay};
use std::{collections::BTreeMap, io};

/// Exact replacement environment, including process-global SDK inputs. Nothing
/// is filled from the current display, default context, HOME or session bus.
pub(super) fn validate_environment(
    session: &BoundSession,
    entries: &[(String, String)],
    helper: &str,
) -> io::Result<()> {
    if !absolute(helper) || entries.len() > 64 {
        return Err(invalid("invalid supplied GIO environment"));
    }
    let mut env = BTreeMap::new();
    let mut bytes = 0;
    for (key, value) in entries {
        bytes += key.len() + value.len();
        let allowed = matches!(
            key.as_str(),
            "DISPLAY"
                | "XAUTHORITY"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "WAYLAND_DISPLAY"
                | "XDG_RUNTIME_DIR"
                | "GIO_LAUNCH_DESKTOP"
                | "LANG"
                | "LC_ALL"
        ) || key.starts_with("THESEUS_FIXTURE_");
        if !allowed
            || key.len() > 128
            || key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || value.len() > 4096
            || value.as_bytes().contains(&0)
            || env.insert(key.as_str(), value.as_str()).is_some()
        {
            return Err(invalid(
                "unapproved/duplicate supplied environment material",
            ));
        }
    }
    if bytes > 16_384 || env.get("GIO_LAUNCH_DESKTOP") != Some(&helper) {
        return Err(invalid(
            "GIO helper environment differs from supplied artifact",
        ));
    }
    let equal = |key: &str, value: &str| env.get(key).is_some_and(|actual| *actual == value);
    let valid = match &session.binding().display {
        SuppliedDisplay::X11 {
            display,
            authority_file,
            bus,
        } => {
            equal("DISPLAY", display)
                && equal("XAUTHORITY", authority_file)
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )
                && !env.contains_key("WAYLAND_DISPLAY")
                && !env.contains_key("XDG_RUNTIME_DIR")
        }
        SuppliedDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => {
            equal("WAYLAND_DISPLAY", socket.as_str())
                && equal("XDG_RUNTIME_DIR", runtime_directory)
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )
                && !env.contains_key("DISPLAY")
                && !env.contains_key("XAUTHORITY")
        }
        _ => false,
    };
    if !valid {
        return Err(invalid(
            "environment differs from bound supplied Linux display/session bus",
        ));
    }
    Ok(())
}
