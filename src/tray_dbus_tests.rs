// Run with dbus-run-session. Never take the real desktop's watcher name.
use super::*;
use crate::{
    app::Daemon,
    config::Config,
    delivery::StaticSink,
    domain::{AudioInput, DeliveryOutcome},
    history::JsonHistoryStore,
    notifier::NoopNotifier,
    recognizer::StaticRecognizer,
    recorder::NoopRecorder,
    recordings::RecordingStore,
};
use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Structure};

struct Watcher(tokio::sync::mpsc::UnboundedSender<String>);
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, name: &str) {
        self.0.send(name.to_owned()).unwrap();
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
fn find_label(layout: Layout, needle: &str) -> Option<i32> {
    if layout
        .1
        .get("label")
        .and_then(|value| <&str>::try_from(value).ok())
        == Some(needle)
    {
        return Some(layout.0);
    }
    layout.2.into_iter().find_map(|child| {
        let structure = Structure::try_from(child).unwrap();
        let layout: Layout = structure.try_into().unwrap();
        find_label(layout, needle)
    })
}
async fn item_id(proxy: &zbus::Proxy<'_>, label: &str) -> i32 {
    let (_, root): (u32, Layout) = proxy
        .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
        .await
        .unwrap();
    find_label(root, label).expect("menu label")
}
async fn click(proxy: &zbus::Proxy<'_>, id: i32) -> zbus::Result<()> {
    proxy
        .call("Event", &(id, "clicked", OwnedValue::from(0_i32), 0_u32))
        .await
}

#[tokio::test]
#[ignore = "requires dbus-run-session and NVSTT_TEST_PRIVATE_BUS=1"]
async fn stale_dbus_events_cannot_select_another_device_or_cancel_a_new_session() {
    assert_eq!(
        std::env::var("NVSTT_TEST_PRIVATE_BUS").as_deref(),
        Ok("1"),
        "use a private bus"
    );
    let (registered, mut registration) = tokio::sync::mpsc::unbounded_channel();
    let bus = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", Watcher(registered))
        .unwrap()
        .build()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("private")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(directory.path().join("history.json"))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    let (controller, join) =
        crate::controller::start(StatusSnapshot::starting(&Config::default()), move || daemon)
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut status = controller.snapshot();
    status.audio.inputs = vec![AudioInput {
        id: "a".to_owned(),
        name: "Input A".to_owned(),
        kind: InputKind::Source,
    }];
    let tray = NvsttTray {
        status: status.clone(),
        controller: controller.clone(),
        host: Arc::new(AtomicBool::new(false)),
        command_note: None,
        pending: false,
        layout_flip: false,
    };
    let handle = tray.spawn().await.unwrap();
    let name = tokio::time::timeout(Duration::from_secs(2), registration.recv())
        .await
        .unwrap()
        .unwrap();
    let proxy = zbus::Proxy::new(&bus, name.as_str(), "/MenuBar", "com.canonical.dbusmenu")
        .await
        .unwrap();
    let old_input = item_id(&proxy, "Input A").await;
    // Same number of rows, changed identity. No old protocol ID may target B.
    status.audio.inputs[0].id = "b".to_owned();
    status.audio.inputs[0].name = "Input B".to_owned();
    status.generation += 1;
    handle
        .update(|tray| tray.update_status(status))
        .await
        .unwrap();
    let new_input = item_id(&proxy, "Input B").await;
    assert_ne!(old_input, new_input);
    assert!(click(&proxy, old_input).await.is_err());
    assert_eq!(
        controller.snapshot().audio.selected,
        InputPreference::SystemDefault
    );
    assert!(controller.request(IpcRequest::Toggle, None).await.is_ok());
    handle
        .update(|tray| tray.update_status(controller.snapshot()))
        .await
        .unwrap();
    let old_cancel = item_id(&proxy, "Cancel recording").await;
    assert!(controller.request(IpcRequest::Cancel, None).await.is_ok());
    handle
        .update(|tray| tray.update_status(controller.snapshot()))
        .await
        .unwrap();
    assert!(controller.request(IpcRequest::Toggle, None).await.is_ok());
    handle
        .update(|tray| tray.update_status(controller.snapshot()))
        .await
        .unwrap();
    assert_ne!(old_cancel, item_id(&proxy, "Cancel recording").await);
    assert!(click(&proxy, old_cancel).await.is_err());
    assert_eq!(controller.snapshot().state, DaemonState::Listening);
    assert!(
        click(&proxy, item_id(&proxy, "Cancel recording").await)
            .await
            .is_ok()
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.snapshot().state != DaemonState::Idle || controller.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await;
    // Exercise normal background service recovery when a host appears late,
    // disappears, and returns on the same private session bus.
    bus.close().await.unwrap();
    let host = Arc::new(AtomicBool::new(false));
    let (stop, stop_rx) = watch::channel(false);
    let service = tokio::spawn(run(controller.clone(), host.clone(), stop_rx));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!host.load(Ordering::Acquire));
    let (registered, mut registration) = tokio::sync::mpsc::unbounded_channel();
    let bus = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", Watcher(registered))
        .unwrap()
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), registration.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !host.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    bus.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (registered, mut registration) = tokio::sync::mpsc::unbounded_channel();
    let _bus = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", Watcher(registered))
        .unwrap()
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), registration.recv())
        .await
        .unwrap()
        .unwrap();
    stop.send_replace(true);
    service.await.unwrap();
    assert!(!host.load(Ordering::Acquire));
    controller.shutdown();
    join.join().unwrap();
}
