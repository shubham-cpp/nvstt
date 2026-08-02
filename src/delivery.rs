use std::process::{Command, Stdio};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{
    domain::DeliveryOutcome,
    error::{AppError, Result},
};

pub trait TextSink: Send {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome>;
}

#[derive(Debug, Default)]
pub struct ClipboardSink;

impl TextSink for ClipboardSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| AppError::Unavailable(format!("wl-copy is unavailable: {error}")))?;

        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Unavailable("wl-copy stdin is unavailable".to_owned()))
            .and_then(|mut stdin| stdin.write_all(text.as_bytes()).map_err(AppError::from));

        if let Err(error) = write_result {
            let _ = child.kill();
            return Err(error);
        }

        let status = child
            .wait()
            .map_err(|error| AppError::Unavailable(format!("wl-copy failed: {error}")))?;
        if !status.success() {
            return Err(AppError::Unavailable(format!(
                "wl-copy exited with status {status}"
            )));
        }

        Ok(DeliveryOutcome::CopiedToClipboard {
            backend: "wl-copy".to_owned(),
        })
    }
}

/// Native Wayland virtual-keyboard delivery.
///
/// `wrtype` performs the protocol negotiation at construction time.  It binds
/// `zwp_virtual_keyboard_manager_v1`, selects the first `wl_seat`, and uploads
/// transient XKB keymaps for the final UTF-8 text.  A compositor can still
/// reject the client after the global is advertised, so callers must retain a
/// fallback sink and treat send errors as capability failures.
pub struct WaylandVirtualKeyboardSink {
    client: wrtype::WrtypeClient,
}

impl WaylandVirtualKeyboardSink {
    pub fn connect() -> Result<Self> {
        let client = wrtype::WrtypeClient::new().map_err(|error| {
            AppError::Unavailable(format!("Wayland virtual keyboard is unavailable: {error}"))
        })?;
        Ok(Self { client })
    }
}

/// Authenticated EI/libei delivery obtained through the XDG RemoteDesktop
/// portal.  The constructor is intentionally separate from daemon startup:
/// connecting to the portal can show a user-consent dialog.
pub struct PortalEiSink {
    client: eitype::EiType,
}

/// Bound the portal handshake so a denied, headless, or broken portal cannot
/// block the CLI toggle forever.  A successful consent dialog normally
/// completes well within this window.  The worker is detached when the
/// timeout fires because Rust cannot safely cancel an in-flight D-Bus call.
const PORTAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn connect_portal_with_timeout(
    config: eitype::EiTypeConfig,
    token: Option<String>,
    thread_name: &str,
) -> std::result::Result<(eitype::EiType, Option<String>), eitype::EiTypeError> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let result = eitype::EiType::connect_portal_with_token(config, token.as_deref());
            let _ = sender.send(result);
        })
        .map_err(|error| eitype::EiTypeError::Connection(error.to_string()))?;

    receiver
        .recv_timeout(PORTAL_CONNECT_TIMEOUT)
        .map_err(|error| {
            let message = match error {
                mpsc::RecvTimeoutError::Timeout => format!(
                    "portal authorization timed out after {} seconds",
                    PORTAL_CONNECT_TIMEOUT.as_secs()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    "portal authorization thread stopped unexpectedly".to_owned()
                }
            };
            eitype::EiTypeError::Connection(message)
        })?
}

impl PortalEiSink {
    fn connect(token_path: Option<&Path>) -> Result<Self> {
        let config = eitype::EiTypeConfig::from_env();
        let saved_token = token_path.and_then(read_restore_token);

        let connect = || -> Result<(eitype::EiType, Option<String>)> {
            if let Some(socket) = env::var_os("LIBEI_SOCKET") {
                return eitype::EiType::connect_socket(&PathBuf::from(socket), config.clone())
                    .map(|client| (client, None))
                    .map_err(|error| {
                        AppError::Unavailable(format!("direct EI socket unavailable: {error}"))
                    });
            }

            // eitype owns a small Tokio runtime for ashpd/zbus.  The daemon
            // itself also runs inside Tokio, so run this blocking portal
            // handshake on a dedicated thread to avoid nested-runtime panic.
            // Bound the wait so a non-responsive portal falls back to the
            // virtual-keyboard and clipboard paths.
            let config = config.clone();
            let token = saved_token.clone();
            connect_portal_with_timeout(config, token, "nvstt-portal-auth").map_err(|error| {
                AppError::Unavailable(format!("portal authorization failed: {error}"))
            })
        };

        let (client, token) = match connect() {
            Ok(result) => result,
            Err(first_error)
                if saved_token.is_some()
                    && env::var_os("LIBEI_SOCKET").is_none()
                    && !portal_authorization_timed_out(&first_error) =>
            {
                // A restore token is single-use and can be revoked.  Retry
                // once without it so the portal can present a fresh consent
                // dialog instead of permanently disabling automatic delivery.
                let config = config.clone();
                let (client, token) = connect_portal_with_timeout(
                    config,
                    None,
                    "nvstt-portal-reauth",
                )
                .map_err(|error| {
                    AppError::Unavailable(format!(
                        "restore token rejected ({first_error}); reauthorization failed: {error}"
                    ))
                })?;
                (client, token)
            }
            Err(error) => {
                return Err(AppError::Unavailable(format!(
                    "EI portal unavailable: {error}"
                )));
            }
        };

        if let (Some(path), Some(token)) = (token_path, token) {
            write_restore_token(path, &token)?;
        }

        Ok(Self { client })
    }
}

impl TextSink for PortalEiSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        if text.trim().is_empty() {
            return Err(AppError::InvalidState(
                "cannot deliver an empty transcript".to_owned(),
            ));
        }

        self.client.type_text(text).map_err(|error| {
            AppError::Unavailable(format!("EI portal rejected the transcript: {error}"))
        })?;

        Ok(DeliveryOutcome::Delivered {
            backend: "xdg-remote-desktop/libei".to_owned(),
        })
    }
}

impl TextSink for WaylandVirtualKeyboardSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        if text.trim().is_empty() {
            return Err(AppError::InvalidState(
                "cannot deliver an empty transcript".to_owned(),
            ));
        }

        self.client.type_text(text).map_err(|error| {
            AppError::Unavailable(format!(
                "Wayland virtual keyboard rejected the transcript: {error}"
            ))
        })?;

        Ok(DeliveryOutcome::Delivered {
            backend: "zwp_virtual_keyboard_v1".to_owned(),
        })
    }
}

/// Selects a compositor-approved native backend first and keeps clipboard as a
/// reliable, user-visible fallback.
///
/// Portal authorization is deliberately attempted only on the first final
/// delivery, so daemon startup never shows a consent dialog. Once connected,
/// the EI session remains alive. If the portal is unavailable or denied, the
/// sink probes the virtual-keyboard global once and keeps that connection for
/// subsequent transcripts. If a compositor drops either connection, the sink
/// discards it and preserves the failure reason in clipboard metadata.
pub struct NativeFirstSink {
    portal: Option<PortalEiSink>,
    portal_attempted: bool,
    virtual_keyboard: Option<WaylandVirtualKeyboardSink>,
    virtual_keyboard_attempted: bool,
    native_error: Option<String>,
    clipboard: ClipboardSink,
    restore_token_path: Option<PathBuf>,
}

impl NativeFirstSink {
    pub fn new() -> Self {
        Self {
            portal: None,
            portal_attempted: false,
            virtual_keyboard: None,
            virtual_keyboard_attempted: false,
            native_error: None,
            clipboard: ClipboardSink,
            restore_token_path: None,
        }
    }

    pub fn new_with_restore_token(path: impl Into<PathBuf>) -> Self {
        let mut sink = Self::new();
        sink.restore_token_path = Some(path.into());
        sink
    }

    fn ensure_portal(&mut self) {
        if self.portal_attempted {
            return;
        }
        self.portal_attempted = true;
        match PortalEiSink::connect(self.restore_token_path.as_deref()) {
            Ok(portal) => self.portal = Some(portal),
            Err(error) => self.native_error = Some(error.to_string()),
        }
    }

    fn ensure_virtual_keyboard(&mut self) {
        if self.virtual_keyboard_attempted || self.virtual_keyboard.is_some() {
            return;
        }
        self.virtual_keyboard_attempted = true;
        match WaylandVirtualKeyboardSink::connect() {
            Ok(keyboard) => self.virtual_keyboard = Some(keyboard),
            Err(error) => {
                self.native_error = Some(match self.native_error.take() {
                    Some(previous) => format!("{previous}; {error}"),
                    None => error.to_string(),
                });
            }
        }
    }
}

impl Default for NativeFirstSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSink for NativeFirstSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        if text.trim().is_empty() {
            return Err(AppError::InvalidState(
                "cannot deliver an empty transcript".to_owned(),
            ));
        }

        self.ensure_portal();
        if let Some(portal) = self.portal.as_mut() {
            match portal.send_final_text(text) {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    self.portal = None;
                    self.native_error = Some(error.to_string());
                }
            }
        }

        self.ensure_virtual_keyboard();
        if let Some(keyboard) = self.virtual_keyboard.as_mut() {
            match keyboard.send_final_text(text) {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    self.virtual_keyboard = None;
                    self.native_error = Some(match self.native_error.take() {
                        Some(previous) => format!("{previous}; {error}"),
                        None => error.to_string(),
                    });
                }
            }
        }

        match self.clipboard.send_final_text(text) {
            Ok(DeliveryOutcome::CopiedToClipboard { backend }) => {
                let backend = match self.native_error.take() {
                    Some(reason) => format!("{backend}; automatic delivery failed: {reason}"),
                    None => backend,
                };
                Ok(DeliveryOutcome::CopiedToClipboard { backend })
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let reason = self
                    .native_error
                    .take()
                    .map(|native| format!("automatic delivery failed: {native}; "))
                    .unwrap_or_default();
                Err(AppError::Unavailable(format!(
                    "{reason}clipboard fallback failed: {error}"
                )))
            }
        }
    }
}

fn read_restore_token(path: &Path) -> Option<String> {
    let token = fs::read_to_string(path).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

fn portal_authorization_timed_out(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Unavailable(message) if message.contains("portal authorization timed out")
    )
}

fn write_restore_token(path: &Path, token: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Unavailable("portal restore-token path has no parent".to_owned())
    })?;
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    set_user_only_permissions(&temporary)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn set_user_only_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct StaticSink {
    pub outcome: Option<DeliveryOutcome>,
}

impl StaticSink {
    pub fn new(outcome: DeliveryOutcome) -> Self {
        Self {
            outcome: Some(outcome),
        }
    }
}

impl TextSink for StaticSink {
    fn send_final_text(&mut self, _text: &str) -> Result<DeliveryOutcome> {
        self.outcome
            .take()
            .ok_or_else(|| AppError::Unavailable("static sink already used".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn restore_token_round_trip_is_user_only() {
        let directory = tempdir().expect("temporary token directory");
        let path = directory.path().join("portal.restore-token");

        write_restore_token(&path, "token-value").expect("write token");
        assert_eq!(read_restore_token(&path).as_deref(), Some("token-value"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path)
                .expect("token metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn empty_restore_token_is_ignored() {
        let directory = tempdir().expect("temporary token directory");
        let path = directory.path().join("portal.restore-token");
        fs::write(&path, "\n  ").expect("write empty token");
        assert!(read_restore_token(&path).is_none());
    }
}
