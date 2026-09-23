# Recent dictation audio implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the seven most recent stopped dictation recordings for private, replayable word-loss diagnosis.

**Architecture:** The existing worker copies drained mono samples before processing. A single recording-store module commits a WAV and metadata after stop, then rotates completed entries. A failed recording never changes the transcription or delivery outcome.

**Tech Stack:** Rust 2024, CPAL, rtrb, serde JSON, std filesystem, existing WAV reader and tempfile tests. Run Rust commands with `mise exec -- cargo ...`.

**Spec:** `docs/superpowers/specs/2026-09-23-recent-dictation-audio-design.md`

## Global constraints

- Save every stopped attempt, including failed capture, failed transcription, and no-speech. Do not save canceled or never-started sessions.
- Retain seven committed entries across restarts, under `$XDG_STATE_HOME/nvstt/recordings/` or the existing XDG fallback.
- Preserve the microphone's drained mono `f32` samples at its original sample rate, before denoise, resampling, VAD, and ASR.
- Keep the CPAL callback unchanged and free of disk writes. Do not hide existing queue loss or change its transcription-failure policy.
- Preserve final-only delivery, the ten-record text history, the public CLI/IPC schema, and the model and gate defaults.
- Use `0700` for private directories and `0600` for WAV and JSON files. Publish complete entries before deleting old entries.
- A recording failure warns in the existing command result. It must not turn successful transcription into failure.
- Do not install or restart the user's active daemon. Do not touch `contrib/nvstt.desktop` or `mise.toml`; both are user-owned untracked files.

## Review focus

- A stop before the first callback yields a valid zero-frame WAV, not a false claim that a speech sample was captured. Test in Task 1 and Task 4.
- Recognition failure during listening does not stop capture draining; the WAV includes audio received afterward. Test in Task 3.
- Queue loss plus recognizer error keeps the capture error primary, marks the WAV partial, and suppresses history and delivery. Test in Task 3 and Task 4.
- Permission or disk-write failure keeps the prior seven entries intact and leaves transcript and delivery status unchanged. Test in Task 2 and Task 4.
- An interrupted save or prune leaves no half-published entry; startup reconciliation removes only owned staging and excess committed entries. Test in Task 2.

## File map

- `src/audio.rs`: float32 WAV writer and reader support; no capture or retention policy.
- `src/recordings.rs` (new): versioned metadata, private atomic entry storage, rotation, startup reconciliation.
- `src/recorder.rs`: expose a read-only post-stop integrity snapshot; keep the existing queue and error result.
- `src/app.rs`: accumulate source samples in the worker, finish capture on every stop, save once, report warnings.
- `src/lib.rs`: export the recording-store module inside this crate.
- `README.md`: document storage, privacy, rotation, discovery, and replay.

### Task 1: Float32 WAV round trip

**Files:** Modify `src/audio.rs:1-95`; add tests to `src/audio.rs`.

**Interfaces:** Produce `pub(crate) fn write_float_wav(writer: &mut impl std::io::Write, sample_rate: i32, samples: &[f32]) -> Result<()>`. Preserve `pub fn read_wav(path: &Path) -> Result<Waveform>`. Task 2 uses the writer.

- [ ] **Step 1: Add a failing float round-trip test.** Keep the PCM tests unchanged.

```rust
#[test]
fn float_wav_keeps_boundary_samples_and_rate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture.wav");
    let values = [0.125_f32, -0.5, 0.25];
    let mut file = std::fs::File::create(&path).unwrap();
    write_float_wav(&mut file, 48_000, &values).unwrap();
    drop(file);
    let wave = read_wav(&path).unwrap();
    assert_eq!(wave.sample_rate, 48_000);
    assert_eq!(wave.samples, values);
}
```

Add a second case using `&[]` and assert a valid zero-frame WAV. Add an unsupported-format case that still fails.

- [ ] **Step 2: Run the focused red test.**

```bash
mise exec -- cargo test --lib audio::tests::float_wav_keeps_boundary_samples_and_rate -- --exact
```

Expected: compile or test failure because float WAV support is absent.

- [ ] **Step 3: Implement a checked RIFF/WAVE writer and extend the reader.**

```rust
let data_bytes = samples.len().checked_mul(4)
    .and_then(|n| u32::try_from(n).ok())
    .filter(|n| *n <= u32::MAX - 48)
    .ok_or_else(|| AppError::Unavailable("recording exceeds WAV size limit".into()))?;
// RIFF size = 48 + data_bytes; fmt size = 16; format = 3 (IEEE float).
// Write a 4-byte fact chunk containing the frame count, then the data chunk.
// channels = 1; bits_per_sample = 32; block_align = 4; byte_rate = sample_rate * 4.
// Write each sample with to_le_bytes(). Reject nonpositive/overflowed sample rates.
// In read_wav, accept (audio_format, bits_per_sample) == (3, 32).
// Decode 4-byte frames with f32::from_le_bytes; retain existing PCM branches.
```

Reject truncated or misaligned float data. Do not silently convert to 16-bit PCM.

- [ ] **Step 4: Run all WAV tests and commit.**

```bash
mise exec -- cargo test --lib audio::tests
mise exec -- cargo test --lib evaluation::tests
git add src/audio.rs && git commit -m "feat: read and write float dictation WAVs"
```

Expected: both groups pass, including existing PCM fixtures.

### Task 2: Private recording store and seven-entry rotation

**Files:** Create `src/recordings.rs`; modify `src/lib.rs` to add `pub mod recordings;`.

**Interfaces:** Define `Recording { pub metadata: RecordingMetadata, pub samples: Vec<f32> }`, `SaveOutcome { pub path: PathBuf, pub retention_warning: Option<String> }`, and `RecordingStore::new(root: PathBuf) -> Self`, `save(&self, recording: &Recording) -> Result<SaveOutcome>`, `reconcile(&self) -> Result<()>`. Task 4 calls `save` and `reconcile`; keep the store concrete, without a new trait.

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CaptureStatus {
    pub dropped_samples: usize,
    pub backend_failed: bool,
    pub duration_exceeded: bool,
    pub stop_failed: bool,
    pub drain_failed: bool,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecordingMetadata {
    pub version: u32, // write 1
    pub session_id: String,
    pub stopped_at_ms: u64,
    pub sample_rate: i32,
    pub frames: usize,
    pub model: String,
    pub streaming_profile: String,
    pub speech_gate: bool,
    pub denoise: bool,
    pub itn: bool,
    pub capture: CaptureStatus,
    pub transcription: crate::domain::TranscriptionStatus,
}
pub struct Recording { pub metadata: RecordingMetadata, pub samples: Vec<f32> }
```

Use `capture` fields to determine completeness. Never serialize transcript text or audio inside metadata.

- [ ] **Step 1: Write store tests first.** Test eight saved entries, startup reconciliation, an abandoned `.staging-` directory, an unrelated file, and private modes.

```rust
let dir = tempfile::tempdir().unwrap();
let store = RecordingStore::new(dir.path().join("recordings"));
for n in 0..8 { store.save(&fixture(n, &[0.125, -0.25])).unwrap(); }
let entries = std::fs::read_dir(dir.path().join("recordings")).unwrap()
    .map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
assert_eq!(entries.len(), 7);
assert!(!entries.iter().any(|p| p.file_name().unwrap().to_string_lossy().ends_with("-0")));
use std::os::unix::fs::PermissionsExt;
assert_eq!(std::fs::metadata(entries[0].join("audio.wav")).unwrap().permissions().mode() & 0o777, 0o600);
```

Define `fixture(n, samples)` as a test helper returning `Recording`, with session ID `format!("1700000000000-{n}")`, stop time `1_700_000_000_000 + n`, sample rate `48_000`, and frame count `samples.len()`; copy the input slice into `Recording.samples`. Set its other metadata fields to `Config::default()` values, `TranscriptionStatus::Succeeded`, and zero/false capture conditions. Add a blocked-root test: a regular file occupies `recordings/`; `save` returns an error and changes no existing entry. Add a failure injection after the first staging file; assert no new committed entry and no pruning. Use an internal, test-only injection point, not a production configuration option. Also inject prune failure after publication; `SaveOutcome.path` must remain valid and `retention_warning` must explain the eighth entry.

- [ ] **Step 2: Run the red store test.**

```bash
mise exec -- cargo test --lib recordings::tests -- --nocapture
```

Expected: compile/test failure until the module and methods exist.

- [ ] **Step 3: Implement the store.** Use a sanitized session ID containing only digits and `-` from `new_session_id()`. Name entries with zero-padded stop time plus ID. Create a private `.staging-<id>` directory in the recording root. Create WAV and JSON with `OpenOptionsExt::mode(0o600)` and `create_new(true)`. Call `write_float_wav`, `sync_all` on both files, then rename the staging directory. After commit, enumerate only valid owned entry directories with parseable version-1 metadata. Sort by stop time and session ID, retain seven, and prune older owned entries. A prune failure returns a valid `SaveOutcome.path` with `retention_warning`, not a false "audio was not saved" error; startup reconciliation retries the prune. In `reconcile`, clean only this module's staging directories and prune excess committed entries. Do not follow or remove unrelated paths or symlinks.

```rust
let payload = serde_json::to_vec_pretty(&recording.metadata)?;
// Validate metadata.frames == recording.samples.len() and sample_rate > 0.
// Write the WAV and payload in the private staging directory.
// Publish by rename in the same parent; prune only after publish.
```

Ensure a failed write cleans its own staging entry and leaves prior committed entries untouched. If cleanup also fails, leave the uncommitted staging directory for startup reconciliation.

- [ ] **Step 4: Run tests and commit.**

```bash
mise exec -- cargo test --lib recordings::tests
mise exec -- cargo test --lib audio::tests
git add src/recordings.rs src/lib.rs && git commit -m "feat: retain seven private dictation audio entries"
```

Expected: rotation, recovery, privacy, and WAV tests pass.

### Task 3: Keep captured samples through worker failures

**Files:** Modify `src/recorder.rs:78-226` and `src/app.rs:32-240`; add tests in both files.

**Interfaces:** Add `CaptureReport` and `AudioSource::capture_report(&self) -> CaptureReport` in `src/recorder.rs`. Add `WorkerResult { audio: Vec<f32>, sample_rate: i32, capture: CaptureReport, drain_failed: bool, outcome: Result<RecognitionOutcome> }` in `src/app.rs`. `RecognitionWorker::finish(capture_only: bool)` returns `(Box<dyn StreamingRecognizer>, WorkerResult)` or a worker-level error. Task 4 consumes this result.

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureReport {
    pub dropped_samples: usize,
    pub backend_failed: bool,
    pub duration_exceeded: bool,
}
// CaptureIntegrity::result() keeps its current error wording and precedence.
```

- [ ] **Step 1: Write failing worker tests.** Start a worker with a small `TestCapture`. Feed `[0.25, 0.5]` before an injected recognizer error and `[0.75]` afterward. Stop the producer, then finish; assert `result.audio == [0.25, 0.5, 0.75]`, no final decode, and the recognizer error. Add queue overflow plus recognizer error: assert `capture.dropped_samples > 0` and the capture error remains primary. Extend `worker_drains_the_last_audio_before_final_flush` to assert its returned audio equals the original 777 samples. Add a test for `finish(true)` to assert no recognizer finalization after a recorder stop error.

```rust
assert_eq!(result.audio, vec![0.25, 0.5, 0.75]);
assert_eq!(result.sample_rate, 16_000);
assert!(result.outcome.is_err());
```

- [ ] **Step 2: Run red tests.**

```bash
mise exec -- cargo test --lib app::tests::worker_drains_the_last_audio_before_final_flush -- --exact
mise exec -- cargo test --lib recorder::tests::queue_loss_is_counted_without_overwriting_audio -- --exact
```

Expected: the extended worker test fails to compile or fails its new audio assertion.

- [ ] **Step 3: Keep draining and separate capture from ASR results.** Snapshot queue entries before processing and append them to worker-owned memory. On a processing error, skip later recognition calls but keep draining and appending until stop. If a source drain fails, set `drain_failed` and retain the samples already collected; do not repeatedly poll a broken source. When `Finish { capture_only: true }` arrives, drain, cancel recognition, and return samples without finalizing. On normal Finish, drain first, check stable integrity, then finish pipeline and ASR only if both capture and worker are healthy. Preserve capture-error precedence. Cancel drops collected memory without saving.

```rust
let samples = source.drain()?;
captured.extend_from_slice(&samples);
if worker_error.is_none() && !samples.is_empty() {
    *worker_error = feed_recognizer(recognizer, pipeline, source.sample_rate(), &samples).err();
}
// After producer stop, drain until empty, then return captured and capture_report.
```

Adapt the existing `finish_worker_session` test to the new result shape. Do not change CPAL callbacks or enlarge the capture queue.

- [ ] **Step 4: Run focused tests and commit.**

```bash
mise exec -- cargo test --lib app::tests
mise exec -- cargo test --lib recorder::tests
git add src/app.rs src/recorder.rs && git commit -m "feat: keep captured audio through recognizer failures"
```

Expected: capture faults still suppress history and delivery; worker preserves the last queued sample.

### Task 4: Save every stopped attempt without changing its outcome

**Files:** Modify `src/app.rs:247-625,666-805` and app tests; optionally add a path helper in `src/paths.rs` only if needed.

**Interfaces:** Pass concrete `RecordingStore` into `Daemon::new` as its final argument. Production `default_daemon` uses `paths.state_dir.join("recordings")`. Keep existing `IpcResponse`, `HistoryRecord`, and CLI types unchanged.

- [ ] **Step 1: Write failing app tests.** Use a `tempdir`-backed store with existing fake recorder, recognizer, history, and sink. Assert one committed entry for success, no speech, empty transcript, backend/queue failure, and recorder stop error. Verify metadata and WAV samples. Use `NoopRecorder` with no callback data to assert a saved zero-frame WAV. Assert no entry for cancel or failed start. Use a store rooted at a regular file to force saving failure: the response retains the original `transcription`, `delivery`, and `ok` values but includes `audio was not saved` in its message.

```rust
let response = daemon.handle(IpcRequest::Toggle); // stop
let IpcResponse::Command { result } = response else { panic!("command") };
assert_eq!(result.transcription, TranscriptionStatus::NoSpeech);
assert!(result.ok);
assert_eq!(saved_entries.len(), 1); // even without text history
```

Keep existing `Effects` spies. For `observed_daemon`, own the temporary directory for the test lifetime; do not rely on a shared path across parallel tests.

- [ ] **Step 2: Run an app red test.**

```bash
mise exec -- cargo test --lib app::tests::no_speech_has_no_delivery_or_history_record -- --exact
```

Expected: the extended test fails its recording assertion before wiring.

- [ ] **Step 3: Implement one stop path.** Stop the producer first. Always call `worker.finish(stop_error.is_some())`, even after a recorder error; preserve that error as the primary transcription failure. Map `WorkerResult.capture` to `CaptureStatus`, including `stop_failed`. Build `RecordingMetadata` with the current session ID and config. Determine transcription outcome before saving, including empty cleaned text; then call `RecordingStore::save` exactly once before returning the command response. On a pre-commit recording failure, append `audio was not saved` to the existing response message without changing its `ok`, `transcription`, `delivery`, transcript, or history effects. On post-commit prune failure, append `old audio could not be pruned` while keeping the saved path valid. Do not save on cancel. Initialize the store and run `reconcile()` in daemon startup; warn on reconciliation errors without preventing dictation.

```rust
let stop_error = self.recorder.stop().err();
let worker = self.finish_recognition_worker(stop_error.is_some());
// Use the worker's audio and capture report even when its recognition outcome fails.
// Save once for this stopped session, then apply the existing no-speech,
// failure, or final-delivery behavior. Append any storage warning to CommandResult.message.
```

If a worker panics before returning samples, preserve the transcription failure and warn that no recording was saved. Keep existing capture error precedence and status transitions.

- [ ] **Step 4: Run app and full tests; commit.**

```bash
mise exec -- cargo test --lib app::tests
mise exec -- cargo test --lib
mise exec -- cargo clippy --all-targets -- -D warnings
git add src/app.rs src/paths.rs && git commit -m "feat: save stopped dictations without changing delivery outcomes"
```

Stage `src/paths.rs` only if it changed. Expected: existing final-only delivery tests still pass; no recording failure becomes a transcription failure.

### Task 5: Document discovery and verify on the host

**Files:** Modify `README.md` near the history/evaluation sections; add an ignored measurement test in `src/recordings.rs`. Do not add a new CLI command.

**Interfaces:** The user can list `$XDG_STATE_HOME/nvstt/recordings/`, select a committed `audio.wav`, and use it in a private evaluation manifest.

- [ ] **Step 1: Write the documentation.** State the seven-entry policy, failed/no-speech coverage, excluded cancellations, directory fallback, non-encrypted files, possible backups, and partial flag. Include a concrete example:

```json
{"id":"boundary-1","audio":"/home/alex/.local/state/nvstt/recordings/1700000000000-1700000000000-1/audio.wav","reference":"hello there","category":"boundary"}
```

Use `$XDG_STATE_HOME/nvstt/recordings` in shell guidance when set; otherwise use `~/.local/state/nvstt/recordings`. Explain that `model evaluate` bypasses live capture and cannot recover speech before the first callback or after stop.

- [ ] **Step 2: Check formatting, tests, and privacy.**

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo test
mise exec -- cargo clippy --all-targets -- -D warnings
git diff --check
```

Expected: all commands pass. Inspect the new module for transcript or audio logging and file modes. Inspect `git status --short` before staging. Keep the two pre-existing untracked user files untouched.

- [ ] **Step 3: Measure storage time.** Add an ignored test in the recording module using its `fixture` helper:

```rust
#[test]
#[ignore]
fn measure_two_minute_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecordingStore::new(dir.path().join("recordings"));
    let samples = vec![0.0_f32; 48_000 * 120];
    let mut times = Vec::new();
    for n in 0..20 {
        let recording = fixture(n, &samples);
        let began = std::time::Instant::now();
        store.save(&recording).unwrap();
        times.push(began.elapsed().as_millis());
    }
    times.sort_unstable();
    println!("p50={} ms p95={} ms", times[9], times[18]);
}
```

```bash
mise exec -- cargo test --lib recordings::tests::measure_two_minute_save -- --ignored --exact --nocapture
```

Record p50/p95 and test conditions in the task report. Time an injected pre-commit save failure with the same fixture. This measures added storage time, not microphone or ASR latency. If p95 exceeds 250 ms, revisit scheduling before shipping. Use consented live audio only if the user asks. Never commit personal audio. WAV and unit tests are not a word-loss fix.

- [ ] **Step 4: Commit the documentation and measurement test.**

```bash
git add README.md src/recordings.rs && git commit -m "docs: explain and measure private recent recordings"
```

Expected: only the intended README and measurement test changes are staged. Present test, latency, and remaining live-microphone limitations to the user. Do not install or restart the active daemon.
