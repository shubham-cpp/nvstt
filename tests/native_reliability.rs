//! Opt-in tests against installed models and a private WAV corpus.
//! See docs/reliability.md for the manifest format. No audio belongs in Git.
use nvstt::{
    audio::read_wav,
    audio_pipeline::{AudioPipeline, MODEL_SAMPLE_RATE},
    config::Config,
    evaluation::{evaluate_model, normalized_words},
    paths::default_paths,
    speech_gate::SpeechGate,
};
use serde::Deserialize;
use std::{env, fs, path::PathBuf};

#[derive(Deserialize)]
struct Expectation {
    id: String,
    contains: Vec<String>,
}

#[test]
#[ignore = "requires NVSTT_REPLAY_MANIFEST, NVSTT_REPLAY_EXPECTATIONS, and installed models"]
fn native_replay_preserves_required_phrases() {
    let manifest = PathBuf::from(env::var("NVSTT_REPLAY_MANIFEST").expect("private manifest"));
    let expectations: Vec<Expectation> = serde_json::from_str(
        &fs::read_to_string(env::var("NVSTT_REPLAY_EXPECTATIONS").expect("expectations path"))
            .unwrap(),
    )
    .unwrap();
    assert!(!expectations.is_empty());
    let model = env::var("NVSTT_REPLAY_MODEL").unwrap_or_else(|_| Config::default().model);
    let mut config = Config::default()
        .with_model_overrides(Some(model), env::var("NVSTT_REPLAY_PROFILE").ok())
        .unwrap();
    config.speech_gate = env::var("NVSTT_REPLAY_GATE").is_ok_and(|s| s == "true");
    let report = evaluate_model(&config, &default_paths().unwrap(), &manifest).unwrap();
    for expected in expectations {
        let clip = report
            .clips
            .iter()
            .find(|c| c.id == expected.id)
            .expect("clip exists");
        let words = normalized_words(&clip.hypothesis);
        for phrase in expected.contains {
            let required = normalized_words(&phrase);
            assert!(!required.is_empty());
            assert!(
                words.windows(required.len()).any(|w| w == required),
                "{} lost a required phrase ({} words)",
                expected.id,
                required.len()
            );
        }
    }
}

#[test]
#[ignore = "requires installed default model or NVSTT_REPLAY_MODEL/PROFILE"]
fn silence_cancel_and_repeat_sessions_do_not_leak_text() {
    use nvstt::recognizer::{RecognitionOutcome, create_recognizer};
    let config = Config::default()
        .with_model_overrides(
            env::var("NVSTT_REPLAY_MODEL").ok(),
            env::var("NVSTT_REPLAY_PROFILE").ok(),
        )
        .unwrap();
    let path = default_paths()
        .unwrap()
        .model_dir
        .join(config.artifact_name());
    let mut recognizer = create_recognizer(&config, path).unwrap();
    recognizer.start_session().unwrap();
    recognizer
        .accept_audio(MODEL_SAMPLE_RATE, &vec![0.0; 317])
        .unwrap();
    recognizer.cancel_session().unwrap();
    for length in [0, 1, 317, 16_000, 48_000] {
        recognizer.start_session().unwrap();
        for chunk in vec![0.0; length].chunks(317) {
            recognizer.accept_audio(MODEL_SAMPLE_RATE, chunk).unwrap();
        }
        assert_eq!(
            recognizer.finish_session().unwrap(),
            RecognitionOutcome::NoSpeech
        );
    }
}

#[test]
#[ignore = "requires NVSTT_VAD_MODEL and NVSTT_REPLAY_WAV"]
fn speech_detection_preserves_every_sample_including_quiet_regions() {
    let mut gate = SpeechGate::from_model_path(env::var("NVSTT_VAD_MODEL").unwrap()).unwrap();
    let wave = read_wav(&PathBuf::from(env::var("NVSTT_REPLAY_WAV").unwrap())).unwrap();
    let mut pipeline = AudioPipeline::new(false);
    let mut expected = pipeline
        .accept_audio(wave.sample_rate, &wave.samples)
        .unwrap();
    expected.extend(pipeline.finish().unwrap());
    for _ in 0..2 {
        gate.start_session();
        let mut actual = Vec::new();
        for chunk in expected.chunks(317) {
            actual.extend(gate.accept_audio(MODEL_SAMPLE_RATE, chunk).unwrap());
        }
        let finish = gate.finish_session();
        actual.extend(finish.samples);
        assert!(finish.speech_detected);
        assert_eq!(
            actual.len(),
            expected.len(),
            "speech detector removed or duplicated audio"
        );
        assert!(actual.iter().zip(&expected).all(|(a, b)| a == b));
    }
}
