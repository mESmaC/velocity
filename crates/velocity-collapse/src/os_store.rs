//! Opens one entry in the OS credential store: Windows Credential Manager, or the Secret
//! Service (GNOME Keyring, KWallet) on Linux.

use crate::CollapseError;

/// The entry for `target`, which names the credential uniquely on every platform.
///
/// Windows keeps `target` as the Credential Manager name, so entries saved by earlier
/// releases, `cmdkey` or the Credential Manager UI are still found.
#[cfg(windows)]
pub(crate) fn entry(
    target: &str,
    service: &str,
    user: &str,
) -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(target, service, user).map_err(credential_error)
}

/// The entry for `target`, which names the credential uniquely on every platform.
///
/// On the Secret Service a target other than `default` names a separate collection, which
/// keyring would create and the desktop would prompt to unlock. The entry stays in the
/// default (login) collection with `target` as its service instead: `service` alone is not
/// unique, since every deploy secret shares one.
#[cfg(target_os = "linux")]
pub(crate) fn entry(
    target: &str,
    service: &str,
    user: &str,
) -> Result<keyring::Entry, CollapseError> {
    let _ = service;
    keyring::Entry::new(target, user).map_err(credential_error)
}

fn credential_error(error: keyring::Error) -> CollapseError {
    CollapseError::Credential(error.to_string())
}
