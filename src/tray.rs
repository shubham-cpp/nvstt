//! Standard StatusNotifierItem and DBusMenu. Desktop hosts choose presentation.
use crate::{
    controller::Controller,
    domain::{AttemptOutcome, DaemonState, InputKind, InputPreference, StatusSnapshot},
    ipc::IpcRequest,
};
use ksni::{
    TrayMethods,
    menu::{RadioGroup, RadioItem, StandardItem, SubMenu},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IconState {
    Ready,
    Listening,
    Transcribing,
    Typing,
    Error,
}

pub(crate) fn icon_state(status: &StatusSnapshot) -> IconState {
    match status.state {
        DaemonState::Listening => IconState::Listening,
        DaemonState::Starting => IconState::Ready,
        DaemonState::Finalizing => IconState::Transcribing,
        DaemonState::Delivering => IconState::Typing,
        DaemonState::Error => IconState::Error,
        DaemonState::Idle
            if status.feedback.initialization_failed
                || !status.audio.available
                || matches!(status.feedback.outcome, Some(AttemptOutcome::Failed { .. })) =>
        {
            IconState::Error
        }
        DaemonState::Idle => IconState::Ready,
    }
}

fn phase(status: &StatusSnapshot) -> &'static str {
    match status.state {
        DaemonState::Starting => "Starting",
        DaemonState::Idle if status.feedback.initialization_failed => "Model unavailable",
        DaemonState::Idle if !status.audio.available => "Input unavailable",
        DaemonState::Idle => "Ready",
        DaemonState::Listening => "Listening",
        DaemonState::Finalizing => "Transcribing",
        DaemonState::Delivering => "Typing",
        DaemonState::Error => "Unavailable",
    }
}

/// No transcript, raw error, model path, or session ID enters the menu.
fn details(status: &StatusSnapshot) -> String {
    let mut lines = vec![
        phase(status).to_owned(),
        format!("Input: {}", menu_text(&status.audio.label())),
    ];
    if status.audio.stale {
        lines.push("Input list needs refresh".to_owned());
    }
    if let Some(outcome) = &status.feedback.outcome {
        lines.push(format!("Last result: {}", outcome.label()));
    }
    if status.feedback.warning.is_some() {
        lines.push("Audio or history warning; check nvstt status".to_owned());
    }
    if status.feedback.command_error.is_some() {
        lines.push("Input selection failed; refresh inputs".to_owned());
    }
    lines.join("\n")
}

// DBusMenu uses underscores as mnemonic markers. Escape names from audio devices.
fn menu_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect::<String>()
        .replace('_', "__")
}

struct NvsttTray {
    status: StatusSnapshot,
    controller: Controller,
    host: Arc<AtomicBool>,
    command_note: Option<String>,
    pending: bool,
    layout_flip: bool,
}

impl NvsttTray {
    fn update_status(&mut self, status: StatusSnapshot) {
        // ksni invalidates DBusMenu item IDs only when the child layout changes.
        // Force that change for each observed generation, including same-count
        // device replacement and a new recording. Old protocol events then fail.
        if self.status.generation != status.generation {
            self.layout_flip = !self.layout_flip;
        }
        self.status = status;
        self.pending = self.controller.is_busy();
    }
}

impl NvsttTray {
    fn submit(&mut self, request: IpcRequest, generation: u64) {
        match self.controller.submit(request, Some(generation)) {
            Ok(reply) => {
                // The controller publishes the result. Dropping the receiver
                // does not cancel an accepted command.
                drop(reply);
                self.pending = true;
                self.command_note = None;
            }
            Err(_) => {
                self.command_note = Some("Action unavailable; open the menu again".to_owned())
            }
        }
    }

    fn microphones(&self) -> Vec<ksni::MenuItem<Self>> {
        let enabled =
            self.status.state == DaemonState::Idle && !self.pending && !self.controller.is_busy();
        let generation = self.status.generation;
        let mut preferences = vec![InputPreference::SystemDefault];
        let mut options = vec![RadioItem {
            label: menu_text(&self.status.audio.default_input.as_ref().map_or_else(
                || "System default (unavailable)".to_owned(),
                |input| format!("System default: {}", input.name),
            )),
            enabled: enabled
                && self.status.audio.default_input.is_some()
                && !self.status.audio.stale,
            ..Default::default()
        }];
        for input in &self.status.audio.inputs {
            preferences.push(InputPreference::Device {
                id: input.id.clone(),
            });
            let suffix = match input.kind {
                InputKind::Monitor => " [monitor]",
                InputKind::Unknown => " [unclassified]",
                InputKind::Source => "",
            };
            let collision = self
                .status
                .audio
                .inputs
                .iter()
                .filter(|other| other.name == input.name)
                .count()
                > 1;
            let identity = if collision {
                format!(" [{}]", menu_text(&input.id))
            } else {
                String::new()
            };
            options.push(RadioItem {
                label: format!("{}{suffix}{identity}", menu_text(&input.name)),
                enabled: enabled && !self.status.audio.stale,
                ..Default::default()
            });
        }
        // Preserve a checked, disabled row when the pinned device disappears.
        if !preferences.contains(&self.status.audio.selected) {
            preferences.push(self.status.audio.selected.clone());
            options.push(RadioItem {
                label: "Selected input (unavailable)".to_owned(),
                enabled: false,
                ..Default::default()
            });
        }
        let selected = preferences
            .iter()
            .position(|p| p == &self.status.audio.selected)
            .unwrap_or(0);
        vec![
            RadioGroup {
                selected,
                options,
                select: Box::new(move |tray: &mut Self, index| {
                    if let Some(preference) = preferences.get(index) {
                        tray.submit(
                            IpcRequest::SelectInput {
                                preference: preference.clone(),
                            },
                            generation,
                        );
                    }
                }),
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Refresh inputs".to_owned(),
                enabled,
                activate: Box::new(move |tray: &mut Self| {
                    tray.submit(IpcRequest::AudioInputs, generation)
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

impl ksni::Tray for NvsttTray {
    const MENU_ON_ACTIVATE: bool = true;
    fn id(&self) -> String {
        "nvstt".to_owned()
    }
    fn title(&self) -> String {
        format!("nvstt: {}", phase(&self.status))
    }
    fn status(&self) -> ksni::Status {
        if icon_state(&self.status) == IconState::Error {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::tray_icons::pixmaps(icon_state(&self.status))
    }
    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::tray_icons::pixmaps(IconState::Error)
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.title(),
            description: details(&self.status),
            ..Default::default()
        }
    }
    fn watcher_online(&self) {
        self.host.store(true, Ordering::Release);
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.host.store(false, Ordering::Release);
        false // Retry registration; successful spawn verifies a host exists.
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let generation = self.status.generation;
        let mut menu = vec![
            StandardItem {
                label: format!("nvstt: {}", phase(&self.status)),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Input: {}", menu_text(&self.status.audio.label())),
                enabled: false,
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Microphone".to_owned(),
                submenu: self.microphones(),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Cancel recording".to_owned(),
                enabled: self.status.state == DaemonState::Listening
                    && !self.pending
                    && !self.controller.is_busy(),
                activate: Box::new(move |tray: &mut Self| {
                    tray.submit(IpcRequest::Cancel, generation)
                }),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(outcome) = &self.status.feedback.outcome {
            menu.push(
                StandardItem {
                    label: format!("Last result: {}", outcome.label()),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.status.feedback.command_error.is_some() {
            menu.push(
                StandardItem {
                    label: "Input action failed; refresh inputs".to_owned(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.status.feedback.warning.is_some() {
            menu.push(
                StandardItem {
                    label: "Audio or history warning; check nvstt status".to_owned(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if let Some(note) = &self.command_note {
            menu.push(
                StandardItem {
                    label: note.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(
            StandardItem {
                label: "Clear last error".to_owned(),
                enabled: self.status.state == DaemonState::Idle
                    && !self.pending
                    && (matches!(
                        self.status.feedback.outcome,
                        Some(AttemptOutcome::Failed { .. })
                    ) || self.status.feedback.command_error.is_some()
                        || self.command_note.is_some()),
                activate: Box::new(move |tray: &mut Self| {
                    tray.command_note = None;
                    tray.submit(IpcRequest::ClearError, generation);
                }),
                ..Default::default()
            }
            .into(),
        );
        // A fixed hidden submenu isolates the layout change from dynamic
        // result rows. Its child count cannot be canceled by another row.
        menu.insert(
            0,
            SubMenu {
                visible: false,
                enabled: false,
                submenu: if self.layout_flip {
                    vec![
                        StandardItem {
                            visible: false,
                            enabled: false,
                            ..Default::default()
                        }
                        .into(),
                    ]
                } else {
                    Vec::new()
                },
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

pub(crate) async fn run(
    controller: Controller,
    host: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) {
    let mut snapshots = controller.snapshots();
    let mut delay = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            break;
        }
        let tray = NvsttTray {
            status: controller.snapshot(),
            controller: controller.clone(),
            host: host.clone(),
            command_note: None,
            pending: false,
            layout_flip: false,
        };
        let spawned = tokio::select! {
            _ = stop.changed() => break,
            result = tokio::time::timeout(Duration::from_secs(5), tray.spawn()) => result,
        };
        match spawned {
            Ok(Ok(handle)) => {
                host.store(true, Ordering::Release);
                delay = Duration::from_secs(1);
                let mut health = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tokio::select! {
                        _ = stop.changed() => break,
                        changed = snapshots.changed() => {
                            if changed.is_err() { break; }
                            let status = snapshots.borrow_and_update().clone();
                            if handle.update(move |tray| { tray.update_status(status); }).await.is_none() { break; }
                        },
                        _ = health.tick() => {
                            if handle.is_closed() || !tray_host_available().await { break; }
                            // Poll also clears a pending rejected request with no snapshot change.
                            let status = controller.snapshot();
                            if handle.update(move |tray| { tray.update_status(status); }).await.is_none() { break; }
                        }
                    }
                }
                host.store(false, Ordering::Release);
                let _ = tokio::time::timeout(Duration::from_secs(2), handle.shutdown()).await;
            }
            _ => {
                host.store(false, Ordering::Release);
                tracing::debug!("tray unavailable; phase notifications remain enabled");
            }
        }
        if *stop.borrow() {
            break;
        }
        tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(delay) => {} }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
    host.store(false, Ordering::Release);
}

async fn tray_host_available() -> bool {
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )
        .await?;
        proxy
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .await
    })
    .await;
    matches!(result, Ok(Ok(true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, domain::FailureStage};

    #[test]
    fn phase_overrides_retained_result_and_failure_remains_at_idle() {
        let mut status = StatusSnapshot::starting(&Config::default());
        status.message = "private".to_owned();
        status.model_path = Some("private".to_owned());
        status.session_id = Some("private".to_owned());
        status.feedback.warning = Some("private".to_owned());
        status.feedback.command_error = Some("private".to_owned());
        status.feedback.outcome = Some(AttemptOutcome::Failed {
            stage: FailureStage::Delivery,
            reason: "private".to_owned(),
        });
        for (state, icon) in [
            (DaemonState::Starting, IconState::Ready),
            (DaemonState::Listening, IconState::Listening),
            (DaemonState::Finalizing, IconState::Transcribing),
            (DaemonState::Delivering, IconState::Typing),
            (DaemonState::Idle, IconState::Error),
        ] {
            status.state = state;
            assert_eq!(icon_state(&status), icon);
        }
        assert!(!details(&status).contains("private"));
        status.audio.available = true;
        status.feedback.outcome = Some(AttemptOutcome::Copied);
        assert_eq!(icon_state(&status), IconState::Ready);
        assert!(details(&status).contains("Copied"));
    }
}

#[cfg(test)]
#[path = "tray_dbus_tests.rs"]
mod dbus_tests;
