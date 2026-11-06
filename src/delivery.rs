use std::process::{Command, Stdio};
use std::{
    env, fs,
    io::{Seek, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
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
        copy_selection(text, false)?;
        Ok(DeliveryOutcome::CopiedToClipboard {
            backend: "wl-copy".to_owned(),
        })
    }
}

fn copy_selection(text: &str, primary: bool) -> Result<()> {
    let mut command = Command::new("wl-copy");
    command.args(["--type", "text/plain;charset=utf-8"]);
    if primary {
        command.arg("--primary");
    }
    run_clipboard_command(&mut command, text, Duration::from_secs(3))
}

fn prepare_paste(text: &str) -> Result<()> {
    // Shift+Insert uses PRIMARY in Kitty/GTK and CLIPBOARD in other clients.
    // Set both before sending any key. Keep the clipboard available for recovery.
    copy_selection(text, false)?;
    copy_selection(text, true)
}

trait PasteKeyboard {
    fn hold_shift(&mut self) -> Result<()>;
    fn tap_insert(&mut self) -> Result<()>;
    fn release_shift(&mut self) -> Result<()>;
}

fn paste_shortcut(keyboard: &mut impl PasteKeyboard) -> Result<()> {
    let result = keyboard.hold_shift().and_then(|()| keyboard.tap_insert());
    // Even a failed send can have reached the compositor. Attempt cleanup on
    // every path; NativeFirstSink then drops failed connections without replay.
    let release = keyboard.release_shift();
    result.and(release)
}

impl PasteKeyboard for eitype::EiType {
    fn hold_shift(&mut self) -> Result<()> {
        self.hold_modifier("shift")
            .map_err(|error| AppError::Unavailable(format!("EI paste modifier: {error}")))
    }

    fn tap_insert(&mut self) -> Result<()> {
        self.press_key("insert")
            .map_err(|error| AppError::Unavailable(format!("EI paste key: {error}")))
    }

    fn release_shift(&mut self) -> Result<()> {
        self.release_modifiers()
            .map_err(|error| AppError::Unavailable(format!("EI modifier release: {error}")))
    }
}

impl PasteKeyboard for wrtype::WrtypeClient {
    fn hold_shift(&mut self) -> Result<()> {
        self.press_modifier(wrtype::Modifier::Shift)
            .map_err(|error| AppError::Unavailable(format!("Wayland paste modifier: {error}")))
    }

    fn tap_insert(&mut self) -> Result<()> {
        self.type_key("Insert")
            .map_err(|error| AppError::Unavailable(format!("Wayland paste key: {error}")))
    }

    fn release_shift(&mut self) -> Result<()> {
        self.release_modifier(wrtype::Modifier::Shift)
            .map_err(|error| AppError::Unavailable(format!("Wayland modifier release: {error}")))
    }
}

fn run_clipboard_command(command: &mut Command, text: &str, timeout: Duration) -> Result<()> {
    // Anonymous input avoids blocking on a pipe if the clipboard tool stalls
    // before reading. No transcript appears in argv or a named temporary file.
    let mut input = tempfile::tempfile()?;
    input.write_all(text.as_bytes())?;
    input.rewind()?;
    let mut child = command
        .stdin(Stdio::from(input))
        .spawn()
        .map_err(|e| AppError::Unavailable(format!("clipboard tool is unavailable: {e}")))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(AppError::Unavailable(format!(
                    "clipboard tool exited with {status}"
                )));
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::Unavailable(match result {
                    Err(error) => format!("clipboard tool failed: {error}"),
                    _ => "clipboard tool timed out".to_owned(),
                }));
            }
        }
    }
}

/// Native Wayland virtual-keyboard delivery.
///
/// `wrtype` performs the protocol negotiation at construction time. It binds
/// `zwp_virtual_keyboard_manager_v1`, selects the first `wl_seat`, and sends
/// the paste shortcut. A compositor can still
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

        prepare_paste(text)?;
        paste_shortcut(&mut self.client)?;

        Ok(DeliveryOutcome::Delivered {
            backend: "xdg-remote-desktop/libei-paste".to_owned(),
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

        prepare_paste(text)?;
        paste_shortcut(&mut self.client)?;

        Ok(DeliveryOutcome::Delivered {
            backend: "zwp_virtual_keyboard_v1-paste".to_owned(),
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
    portal: Option<Box<dyn TextSink>>,
    portal_attempted: bool,
    virtual_keyboard: Option<Box<dyn TextSink>>,
    virtual_keyboard_attempted: bool,
    native_error: Option<String>,
    clipboard: Box<dyn TextSink>,
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
            clipboard: Box::new(ClipboardSink),
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
            Ok(portal) => self.portal = Some(Box::new(portal)),
            Err(error) => self.native_error = Some(error.to_string()),
        }
    }

    fn ensure_virtual_keyboard(&mut self) {
        if self.virtual_keyboard_attempted || self.virtual_keyboard.is_some() {
            return;
        }
        self.virtual_keyboard_attempted = true;
        match WaylandVirtualKeyboardSink::connect() {
            Ok(keyboard) => self.virtual_keyboard = Some(Box::new(keyboard)),
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
                    self.portal_attempted = false;
                    self.native_error = Some(error.to_string());
                    // The failed send may have inserted a prefix. Never
                    // replay it through another automatic backend.
                    return self.copy_fallback(text);
                }
            }
        }

        self.ensure_virtual_keyboard();
        if let Some(keyboard) = self.virtual_keyboard.as_mut() {
            match keyboard.send_final_text(text) {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    self.virtual_keyboard = None;
                    self.virtual_keyboard_attempted = false;
                    self.native_error = Some(match self.native_error.take() {
                        Some(previous) => format!("{previous}; {error}"),
                        None => error.to_string(),
                    });
                    return self.copy_fallback(text);
                }
            }
        }

        self.copy_fallback(text)
    }
}

impl NativeFirstSink {
    fn copy_fallback(&mut self, text: &str) -> Result<DeliveryOutcome> {
        match self.clipboard.send_final_text(text) {
            Ok(DeliveryOutcome::CopiedToClipboard { backend }) => {
                let backend = match self.native_error.as_ref() {
                    Some(reason) => format!("{backend}; automatic delivery failed: {reason}"),
                    None => backend,
                };
                Ok(DeliveryOutcome::CopiedToClipboard { backend })
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let reason = self
                    .native_error
                    .as_ref()
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

    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| AppError::from(error.error))?;
    fs::File::open(parent)?.sync_all()?;
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
    fn paste_shortcut_releases_shift_on_every_failure_without_repeating_insert() {
        struct Keyboard {
            fail: Option<&'static str>,
            events: Vec<&'static str>,
        }
        impl Keyboard {
            fn event(&mut self, name: &'static str) -> Result<()> {
                self.events.push(name);
                if self.fail == Some(name) {
                    Err(AppError::Unavailable(name.into()))
                } else {
                    Ok(())
                }
            }
        }
        impl PasteKeyboard for Keyboard {
            fn hold_shift(&mut self) -> Result<()> {
                self.event("hold")
            }
            fn tap_insert(&mut self) -> Result<()> {
                self.event("insert")
            }
            fn release_shift(&mut self) -> Result<()> {
                self.event("release")
            }
        }
        for failure in [None, Some("hold"), Some("insert"), Some("release")] {
            let mut keyboard = Keyboard {
                fail: failure,
                events: Vec::new(),
            };
            assert_eq!(paste_shortcut(&mut keyboard).is_ok(), failure.is_none());
            let expected = if failure == Some("hold") {
                vec!["hold", "release"]
            } else {
                vec!["hold", "insert", "release"]
            };
            assert_eq!(keyboard.events, expected);
        }
    }

    #[test]
    fn partial_native_failure_copies_once_without_retyping() {
        struct PartialFailure;
        impl TextSink for PartialFailure {
            fn send_final_text(&mut self, _: &str) -> Result<DeliveryOutcome> {
                Err(AppError::Unavailable("failed after a prefix".into()))
            }
        }
        struct MustNotType;
        impl TextSink for MustNotType {
            fn send_final_text(&mut self, _: &str) -> Result<DeliveryOutcome> {
                panic!("must not type a second copy after a partial failure")
            }
        }
        let mut sink = NativeFirstSink::new();
        sink.portal_attempted = true;
        sink.portal = Some(Box::new(PartialFailure));
        sink.virtual_keyboard = Some(Box::new(MustNotType));
        sink.clipboard = Box::new(StaticSink::new(DeliveryOutcome::CopiedToClipboard {
            backend: "test".into(),
        }));
        let outcome = sink.send_final_text("complete transcript").unwrap();
        assert!(matches!(outcome, DeliveryOutcome::CopiedToClipboard { .. }));
        assert!(!sink.portal_attempted, "reconnect on a later attempt");
    }

    #[test]
    fn clipboard_stall_has_a_deadline_even_with_large_input() {
        let started = Instant::now();
        let error = run_clipboard_command(
            Command::new("sleep").arg("10"),
            &"x".repeat(1_000_000),
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

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
