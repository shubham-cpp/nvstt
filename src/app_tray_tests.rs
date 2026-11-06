use super::*;
use crate::{
    audio_input::InputCatalog,
    delivery::StaticSink,
    domain::{AudioInput, InputKind},
    notifier::NoopNotifier,
};
use std::sync::Mutex;

#[derive(Clone)]
struct Inputs {
    devices: Vec<AudioInput>,
    default: Option<String>,
}
struct MutableInputs(Arc<Mutex<Inputs>>);
impl InputProvider for MutableInputs {
    fn catalog(&mut self) -> Result<InputCatalog> {
        let inputs = self.0.lock().unwrap().clone();
        Ok(InputCatalog {
            default_input: inputs
                .devices
                .iter()
                .find(|input| Some(&input.id) == inputs.default.as_ref())
                .cloned(),
            inputs: inputs.devices,
        })
    }
    fn resolve(&mut self, preference: &InputPreference) -> Result<AudioInput> {
        let inputs = self.0.lock().unwrap();
        let id = match preference {
            InputPreference::SystemDefault => inputs.default.as_ref(),
            InputPreference::Device { id } => Some(id),
        };
        inputs
            .devices
            .iter()
            .find(|input| Some(&input.id) == id)
            .cloned()
            .ok_or_else(|| AppError::Unavailable("missing test input".to_owned()))
    }
}
fn input(id: &str, kind: InputKind) -> AudioInput {
    AudioInput {
        id: id.to_owned(),
        name: "Same microphone".to_owned(),
        kind,
    }
}

struct SelectedRecorder {
    inner: NoopRecorder,
    selections: Arc<Mutex<Vec<InputPreference>>>,
}
impl Recorder for SelectedRecorder {
    fn select_input(&mut self, preference: InputPreference) -> Result<()> {
        self.selections.lock().unwrap().push(preference);
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }
    fn stop(&mut self) -> Result<CaptureReport> {
        self.inner.stop()
    }
    fn cancel(&mut self) -> Result<()> {
        self.inner.cancel()
    }
    fn audio_source(&mut self) -> Result<AudioSource> {
        self.inner.audio_source()
    }
}

#[test]
fn default_tracks_next_session_and_pinned_loss_never_substitutes() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = Arc::new(Mutex::new(Inputs {
        devices: vec![
            input("a", InputKind::Source),
            input("b", InputKind::Monitor),
        ],
        default: Some("a".to_owned()),
    }));
    let selections = Arc::new(Mutex::new(Vec::new()));
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(SelectedRecorder {
            inner: NoopRecorder::default(),
            selections: selections.clone(),
        }),
        Box::new(StaticRecognizer::new("final transcript")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(directory.path().join("history.json"))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.inputs = Box::new(MutableInputs(inputs.clone()));
    daemon.initialize();
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(daemon.status.audio.active.as_ref().unwrap().id, "a");
    let generation = daemon.status.generation;
    assert!(
        !daemon
            .handle(IpcRequest::SelectInput {
                preference: InputPreference::Device { id: "b".to_owned() }
            })
            .is_ok()
    );
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
    inputs.lock().unwrap().default = Some("b".to_owned());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(
        daemon.status.audio.active.as_ref().unwrap().kind,
        InputKind::Monitor
    );
    assert_eq!(daemon.status.audio.default_input.as_ref().unwrap().id, "b");
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
    assert!(daemon.status.generation > generation);
    assert!(
        daemon
            .handle(IpcRequest::SelectInput {
                preference: InputPreference::Device { id: "b".to_owned() }
            })
            .is_ok()
    );
    inputs
        .lock()
        .unwrap()
        .devices
        .retain(|input| input.id != "b");
    inputs.lock().unwrap().default = Some("a".to_owned());
    assert!(!daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(
        daemon.status.audio.selected,
        InputPreference::Device { id: "b".to_owned() }
    );
    assert!(!daemon.status.audio.available);
    assert!(daemon.status.audio.label().contains("unavailable"));
    assert_eq!(
        selections.lock().unwrap().last(),
        Some(&InputPreference::Device { id: "b".to_owned() })
    );
    assert_eq!(
        fs::read_dir(directory.path().join("recordings"))
            .unwrap()
            .count(),
        0
    );
    assert!(daemon.history.list(10).unwrap().is_empty());
    assert!(
        daemon
            .handle(IpcRequest::SelectInput {
                preference: InputPreference::SystemDefault
            })
            .is_ok()
    );
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(daemon.status.audio.active.as_ref().unwrap().id, "a");
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
}

struct ReplaySink;
impl TextSink for ReplaySink {
    fn send_final_text(&mut self, _text: &str) -> Result<DeliveryOutcome> {
        Ok(DeliveryOutcome::Delivered {
            backend: "verification-only".to_owned(),
        })
    }
}

struct ReplayRecorder {
    samples: Arc<Mutex<(i32, Vec<f32>)>>,
    source: Option<AudioSource>,
}
impl Recorder for ReplayRecorder {
    fn start(&mut self) -> Result<()> {
        let samples = self.samples.lock().unwrap();
        self.source = Some(AudioSource::test_source(samples.0, samples.1.clone()));
        Ok(())
    }
    fn stop(&mut self) -> Result<CaptureReport> {
        Ok(CaptureReport::default())
    }
    fn cancel(&mut self) -> Result<()> {
        self.source.take();
        Ok(())
    }
    fn audio_source(&mut self) -> Result<AudioSource> {
        self.source
            .take()
            .ok_or_else(|| AppError::Unavailable("missing replay source".to_owned()))
    }
}

/// Run only with a private snapshot of the user's recordings. The production
/// daemon, history, recording directory, and delivery backend are never used.
#[test]
#[ignore = "requires NVSTT_VERIFY_ROOT with a private audio history manifest"]
fn verify_saved_audio_history_through_daemon() {
    let root = std::path::PathBuf::from(
        env::var_os("NVSTT_VERIFY_ROOT").expect("private verification root"),
    );
    let run = root.join(format!("daemon-replay-{}", new_session_id()));
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = crate::paths::default_paths().unwrap();
    let config = Config::load(&paths.config_path).unwrap();
    let recognizer =
        create_recognizer(&config, paths.model_dir.join(config.artifact_name())).unwrap();
    let samples = Arc::new(Mutex::new((48_000, Vec::new())));
    let mut daemon = Daemon::new(
        config,
        Box::new(ReplayRecorder {
            samples: samples.clone(),
            source: None,
        }),
        recognizer,
        Box::new(ReplaySink),
        Box::new(JsonHistoryStore::new(run.join("history.json"))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(run.join("recordings")),
    );
    daemon.initialize();
    let mut report = Vec::new();
    for line in fs::read_to_string(root.join("manifest.jsonl"))
        .unwrap()
        .lines()
    {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        let wave = crate::audio::read_wav(Path::new(entry["audio"].as_str().unwrap())).unwrap();
        *samples.lock().unwrap() = (wave.sample_rate, wave.samples.clone());
        assert!(
            daemon.handle(IpcRequest::Toggle).is_ok(),
            "replay start failed"
        );
        let session = daemon.status.session_id.clone().unwrap();
        let response = daemon.handle(IpcRequest::Toggle);
        let IpcResponse::Command { result } = response else {
            panic!("replay returned no result");
        };
        fs::write(
            root.join("last-replay-result.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        assert!(
            result.ok,
            "replay completion failed; private result recorded"
        );
        let saved_path = fs::read_dir(run.join("recordings"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                let metadata: crate::recordings::RecordingMetadata =
                    serde_json::from_slice(&fs::read(path.join("metadata.json")).unwrap()).unwrap();
                metadata.session_id == session
            })
            .expect("saved replay entry");
        let saved = crate::audio::read_wav(&saved_path.join("audio.wav")).unwrap();
        assert!(
            saved.sample_rate == wave.sample_rate && saved.samples == wave.samples,
            "saved original audio changed"
        );
        assert!(
            result.transcript.as_deref() == entry["reference"].as_str(),
            "replayed transcript differs from saved history"
        );
        assert!(matches!(
            result.status.feedback.outcome,
            Some(AttemptOutcome::Delivered)
        ));
        report.push(serde_json::json!({ "id":entry["id"], "sample_rate":saved.sample_rate, "frames":saved.samples.len(), "original_audio_identical":true, "transcript_matches_history":true, "delivery":"verification-only" }));
    }
    assert_eq!(report.len(), 7, "expected seven preserved history clips");
    assert_eq!(daemon.history.list(10).unwrap().len(), 7);
    assert_eq!(fs::read_dir(run.join("recordings")).unwrap().count(), 7);
    fs::write(
        root.join("daemon-replay-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn completion_snapshot_keeps_storage_warning_and_failed_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("recordings");
    fs::write(&blocked, b"owned fixture").unwrap();
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("final transcript")),
        Box::new(StaticSink::new(DeliveryOutcome::Failed {
            reason: "test backend unavailable".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(directory.path().join("history.json"))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(blocked),
    );
    daemon.initialize();
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("expected completion");
    };
    assert_eq!(
        result.status.feedback.warning.as_deref(),
        Some("audio was not saved")
    );
    assert!(matches!(
        result.status.feedback.outcome,
        Some(AttemptOutcome::Failed {
            stage: FailureStage::Delivery,
            ..
        })
    ));
    assert_eq!(
        crate::tray::icon_state(&result.status),
        crate::tray::IconState::Error
    );
    assert!(daemon.handle(IpcRequest::ClearError).is_ok());
    assert_eq!(
        crate::tray::icon_state(&daemon.status),
        crate::tray::IconState::Ready
    );
    assert!(daemon.status.feedback.warning.is_some());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.status.feedback.outcome.is_none() && daemon.status.feedback.warning.is_none());
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
}
