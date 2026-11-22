//! Native desktop notifications for high-signal project health alerts (#616).

const SOURCE: &str = "desktop notification";

/// Dispatches one native desktop notification in the background, logging any failure to
/// diagnostics without blocking the caller.
pub(crate) fn send(title: String, body: String) {
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = send_blocking(&title, &body) {
            crate::diagnostics::record(SOURCE, &format!("Could not show notification: {error}"));
        }
    });
}

#[cfg(windows)]
fn send_blocking(title: &str, body: &str) -> Result<(), String> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;

    let xml = velocity_core::project_health::windows_toast_xml(title, body);
    let document = XmlDocument::new().map_err(|error| error.to_string())?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|error| error.to_string())?;
    let toast =
        ToastNotification::CreateToastNotification(&document).map_err(|error| error.to_string())?;
    // Prefer Velocity's installed AppUserModelID; fall back to Windows PowerShell's
    // always-registered AUMID when running from a debug or portable build without a
    // registered Start Menu shortcut.
    const AUMIDS: [&str; 2] = [
        "dev.velocity.ide",
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe",
    ];
    let mut last_error = None;
    for aumid in AUMIDS {
        match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))
            .and_then(|notifier| notifier.Show(&toast))
        {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(last_error.unwrap_or_else(|| "Could not show Windows toast notification".into()))
}

/// Shows the alert through the freedesktop notification service (GNOME Shell, KDE Plasma,
/// dunst and others).
#[cfg(target_os = "linux")]
fn send_blocking(title: &str, body: &str) -> Result<(), String> {
    notify_rust::Notification::new()
        .appname("Velocity")
        .summary(title)
        .body(body)
        .show()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn send_blocking(_title: &str, _body: &str) -> Result<(), String> {
    Ok(())
}
