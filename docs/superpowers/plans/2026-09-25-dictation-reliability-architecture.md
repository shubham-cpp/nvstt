# Dictation reliability implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make stopped-audio retention truthful and easier to diagnose. Do not change recognition defaults or claim that this fixes missing words.

**Architecture:** The recorder reports shutdown separately from capture integrity. The worker still checks its final capture report. The recording store creates disk metadata and returns typed warnings. The daemon records stop time before waiting for the worker, saves audio, then handles the existing delivery result.

**Tech Stack:** Rust 2024; CPAL 0.18.1; rtrb; serde; existing WAV and evaluation code; tempfile tests. Use `mise exec -- cargo ...`.

**Spec:** `docs/superpowers/specs/2026-09-25-dictation-reliability-architecture-design.md`.

## Verified starting point and limits

- Baseline: `mise exec -- cargo test --quiet` passed 142 tests; two were ignored before this plan.
- Strict formatting fails in `src/dictation_transcript.rs`, `src/recorder.rs`, and `src/speech_gate.rs`. Strict Clippy reports two test-table type-complexity errors in `src/dictation_transcript.rs:301,406`.
- `src/recorder.rs:52-62,348-358` returns capture-integrity errors from `stop()` after producer shutdown. `src/app.rs:487-539` labels every such error `stop_failed`.
- `src/app.rs:487-527,716-722` reads stop time after joining the worker. `src/recordings.rs:146-166` prunes even if the published root cannot sync.
- `src/app.rs:519-563` constructs disk version and frame count. It detects prune warnings by searching warning text.
- The prior 50.83 ms p95 synthetic difference is in `.superpowers/sdd/2026-09-23-recent-dictation-audio/final-fix-report.md:29-34`. It is not live capture or insertion latency.
- CPAL 0.18.1 Linux ALSA, PipeWire, and PulseAudio stream destructors join non-current worker threads. See their `src/host/{alsa/mod.rs,pipewire/stream.rs,pulseaudio/stream.rs}` implementations. Do not generalize that finding to other backends.
- The evaluator feeds 20 ms chunks and simulates backlog. It cannot measure microphone callback timing, queue loss, or desktop insertion. It has no first-word metric.
- The ignored benchmark signals readiness after its first recognizer `accept_audio`, not necessarily after all source processing. Its stop-to-result timing may include worker tail work.

## Global Constraints

- Save the same stopped sessions: success, no speech, capture failure, and recognition failure. Exclude cancel and failed start.
- Keep original mono `f32` audio before denoise, resampling, VAD, or ASR. Keep the bounded queue and 30-minute capture limit.
- Save before text delivery. A save failure warns without changing transcription or delivery status.
- Keep the seven-entry WAV archive and ten-entry text history. Keep the current CLI and IPC shapes.
- Keep private directory/file permissions, atomic publication, symlink checks, and owned-only cleanup.
- Do not add a history power-loss sync. Do not change the model or speech-gate default.
- Keep personal audio, corrected references, and reports outside Git. Do not rebuild, install, or restart the linked binary or daemon.
- Leave the main checkout and its untracked `contrib/nvstt.desktop` and `mise.toml` unchanged. Preserve the two untracked worktree research notes.

## Review Focus

- Seven existing entries, then root sync fails: keep the old seven and the new path. Task 1 tests recovery and foreign files.
- Two stops share a millisecond across sequence `9` to `10`: retain the newer entry. Task 2 tests numeric ordering.
- Capture loses queued samples after shutdown: mark capture loss, not stop failure. Task 3 tests the recorder and daemon.
- Recognition finalization stalls after stop: store the earlier stop time. Task 3 uses a bounded channel barrier.
- A partial final gate frame follows speech: forward its padded boundary once. Task 4 tests framing and routing together.

---

## File map

- `src/recordings.rs`: prevent pruning after root-sync failure; own disk metadata and typed save warnings.
- `src/recorder.rs`: return a post-stop capture snapshot, not a capture-integrity error, on successful shutdown.
- `src/app.rs`: record stop time before worker completion; assemble archive input; render typed warnings.
- `src/speech_gate.rs`: add a boundary/finalization characterization test. Keep the gate policy unchanged.
- `src/dictation_transcript.rs`: fix two existing Clippy warnings in tests; apply formatting only elsewhere.
- `README.md`, `CONTEXT.md`, `docs/adr/0002-persist-bounded-text-history.md`, and new `docs/adr/0014-retain-recent-dictation-audio.md`: state the current retention and paired diagnostic rules.

### Task 1: Do not prune if the published root fails to sync

**Files:** `src/recordings.rs`.

- [ ] **Step 1: Add a failing, seven-entry regression test.** Use the existing `fixture`, `entries`, and sync-failure test hooks.

```rust
#[test]
fn failed_root_sync_keeps_all_previous_entries_until_reconcile() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("recordings");
    let healthy = RecordingStore::new(root.clone());
    for n in 0..7 {
        healthy.save(&fixture(n, &[0.125])).unwrap();
    }
    let foreign = root.join("notes.txt");
    fs::write(&foreign, b"keep").unwrap();
    let mut failing = RecordingStore::new(root.clone());
    failing.fail_root_dir_sync = true;
    let saved = failing.save(&fixture(7, &[0.25])).unwrap();
    assert!(saved.path.join("audio.wav").is_file());
    assert!(saved.retention_warning.as_deref().unwrap().contains("directory sync"));
    assert_eq!(failing.owned_entries().unwrap().len(), 8);
    assert!(entries(&root).iter().any(|p| p.file_name().unwrap().to_string_lossy().ends_with("-0")));
    healthy.reconcile().unwrap();
    assert_eq!(healthy.owned_entries().unwrap().len(), 7);
    assert!(saved.path.exists());
    assert_eq!(fs::read(foreign).unwrap(), b"keep");
}
```

- [ ] **Step 2: Run the red test.** Expect eight-entry assertion failure: the current save prunes the oldest entry.

```bash
mise exec -- cargo test --lib recordings::tests::failed_root_sync_keeps_all_previous_entries_until_reconcile -- --exact
```

- [ ] **Step 3: Return the saved path and warning immediately if root sync fails.** Do not call `prune` in that branch. Keep staging sync, rename, and normal prune order unchanged.

```rust
if let Err(error) = root_sync {
    return Ok(SaveOutcome {
        path,
        retention_warning: Some(format!("recording saved, but directory sync failed: {error}")),
    });
}
```

- [ ] **Step 4: Run `mise exec -- cargo test --lib recordings::tests`.** Check the path, prior seven entries, recovery, and existing permission tests.
- [ ] **Step 5: Commit only this change:** `git add src/recordings.rs && git commit -m "fix: keep prior audio when archive sync fails"`.

### Task 2: Give the archive store one typed input and typed warnings

**Files:** `src/recordings.rs`, `src/app.rs`.

**Interface:** Replace the old `Recording { metadata, samples }` input and prose `retention_warning` with these types. Keep the on-disk `RecordingMetadata` JSON fields and version unchanged.

```rust
pub struct RecordingSettings {
    pub model: String,
    pub streaming_profile: String,
    pub speech_gate: bool,
    pub denoise: bool,
    pub itn: bool,
}

pub struct RecordingInput {
    pub session_id: String,
    pub stopped_at_ms: u64,
    pub sample_rate: i32,
    pub settings: RecordingSettings,
    pub capture: CaptureStatus,
    pub transcription: TranscriptionStatus,
    pub samples: Vec<f32>,
}

pub enum SaveWarning {
    DirectorySyncFailed(AppError),
    PruneFailed(AppError),
}

pub struct SaveOutcome {
    pub path: PathBuf,
    pub warnings: Vec<SaveWarning>,
}
```

- [ ] **Step 1: Add archive tests using `RecordingInput`.** This is a compile-failing interface test before implementation.
- [ ] **Step 2: Assert metadata version `1`, frame count from `samples.len()`, settings, and an unchanged WAV.** Add type-pattern checks for both sync and prune warning hooks. Add an equal-stop-time retention test. Give IDs `3` through `10` the same `stopped_at_ms`. Assert `3` expires and `10` remains. The current stale check rejects `10`.

```rust
let mut input = fixture(10, &[]);
input.stopped_at_ms = 1_700_000_000_000;
let saved = store.save(&input).unwrap();
assert!(saved.path.exists());
assert_eq!(store.owned_entries().unwrap().len(), 7);
assert!(!store.owned_entries().unwrap().iter().any(|entry| entry.2.ends_with("-3")));
```

Set the same stop time for each seed input. Check `DirectorySyncFailed` and `PruneFailed` with `matches!` on `saved.warnings.iter()`.
- [ ] **Step 3: Run the focused red tests:** `mise exec -- cargo test --lib recordings::tests`. Expect missing-type errors.
- [ ] **Step 4: Build `RecordingMetadata` only in `RecordingStore::save`.** Keep existing ID, rate, WAV-size, stale-entry, and symlink checks. Keep owned-entry parsing unchanged. Derive disk-only fields:

```rust
let metadata = RecordingMetadata {
    version: 1,
    session_id: recording.session_id.clone(),
    stopped_at_ms: recording.stopped_at_ms,
    sample_rate: recording.sample_rate,
    frames: recording.samples.len(),
    model: recording.settings.model.clone(),
    streaming_profile: recording.settings.streaming_profile.clone(),
    speech_gate: recording.settings.speech_gate,
    denoise: recording.settings.denoise,
    itn: recording.settings.itn,
    capture: recording.capture.clone(),
    transcription: recording.transcription,
};
```

Compare numeric ID components for equal stop times in both stale checks and rotation. This avoids integer overflow and keeps the stored ID format.

```rust
fn cmp_decimal(a: &str, b: &str) -> std::cmp::Ordering {
    let left = a.trim_start_matches('0');
    let right = b.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}
fn cmp_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_start, a_seq) = a.split_once('-').expect("validated session ID");
    let (b_start, b_seq) = b.split_once('-').expect("validated session ID");
    cmp_decimal(a_start, b_start)
        .then_with(|| cmp_decimal(a_seq, b_seq))
        .then_with(|| a.cmp(b))
}
// Replace the tuple comparison in the stale-entry check:
let older = metadata.stopped_at_ms < oldest_retained.1
    || metadata.stopped_at_ms == oldest_retained.1
        && cmp_ids(&metadata.session_id, &oldest_retained.2).is_lt();
// Replace the owned_entries tuple sort:
owned.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| cmp_ids(&a.2, &b.2)));
```

- [ ] **Step 5: Return `DirectorySyncFailed` without pruning; return `PruneFailed` after a failed prune.** Keep a published path in both outcomes.

```rust
if let Err(error) = root_sync {
    return Ok(SaveOutcome { path, warnings: vec![SaveWarning::DirectorySyncFailed(error)] });
}
let warnings = self.prune().err().map(SaveWarning::PruneFailed).into_iter().collect();
Ok(SaveOutcome { path, warnings })
```

- [ ] **Step 6: Update daemon construction and warning rendering.** Copy current config fields into `RecordingSettings`; match `SaveWarning` variants. Do not search error strings. Keep `CommandResult` and text-history flow unchanged.

```rust
let recording = RecordingInput {
    session_id: self.status.session_id.clone().expect("listening session ID"),
    stopped_at_ms: now_ms(), // Task 3 moves this read to immediately after stop.
    sample_rate: worker.sample_rate,
    settings: RecordingSettings {
        model: self.config.model.clone(),
        streaming_profile: self.config.streaming_profile.clone(),
        speech_gate: self.config.speech_gate,
        denoise: self.config.denoise,
        itn: self.config.itn,
    },
    capture: CaptureStatus {
        dropped_samples: worker.capture.dropped_samples,
        backend_failed: worker.capture.backend_failed,
        duration_exceeded: worker.capture.duration_exceeded,
        stop_failed,
        drain_failed: worker.drain_failed,
    },
    transcription: status,
    samples: worker.audio,
};
// In the existing save match, render warning variants by kind:
let warnings = saved.warnings.into_iter().map(|warning| match warning {
    SaveWarning::DirectorySyncFailed(error) =>
        format!("recording saved, but directory sync failed: {error}"),
    SaveWarning::PruneFailed(error) =>
        format!("old audio could not be pruned; retention prune failed: {error}"),
}).collect::<Vec<_>>();
let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
```

- [ ] **Step 7: Add daemon tests for both post-publication warnings.** Expose test-only setters for the existing sync and prune hooks. Set one after `observed_daemon()` calls `initialize()`. Keep the existing pre-publication save-failure tests.

```rust
// In the RecordingStore impl:
#[cfg(test)]
pub(crate) fn fail_root_sync_for_test(&mut self) {
    self.fail_root_dir_sync = true;
}
#[cfg(test)]
pub(crate) fn fail_prune_for_test(&mut self) {
    self.fail_prune = true;
}

// In app::tests, stop each normal session:
for sync_failure in [true, false] {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    if sync_failure {
        daemon.recordings.fail_root_sync_for_test();
    } else {
        daemon.recordings.fail_prune_for_test();
    }
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("expected command");
    };
    assert!(result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
    assert_eq!(result.delivery, DeliveryStatus::Delivered);
    assert_eq!(effects.lock().unwrap().sent.len(), 1);
    assert_eq!(saved_recordings(&directory).len(), 1);
    assert!(result.message.contains(if sync_failure {
        "directory sync failed"
    } else {
        "old audio could not be pruned"
    }));
}
```

Run the same loop with `daemon.recognizer = Some(Box::new(StaticRecognizer::no_speech()))` before the first toggle. Assert `result.ok`, `NoSpeech`, `NotAttempted`, no text history, and one saved WAV.

- [ ] **Step 8: Adapt existing invalid-input tests.** Test invalid session ID and sample rate. Test derived version and frames instead of injecting impossible caller-supplied metadata.
- [ ] **Step 9: Run `mise exec -- cargo test --lib recordings::tests` and `mise exec -- cargo test --lib app::tests`.** Check save-failure/no-speech response tests.
- [ ] **Step 10: Commit:** `git add src/recordings.rs src/app.rs && git commit -m "refactor: make recording store own metadata and warnings"`.

### Task 3: Separate producer shutdown from capture integrity and stamp stop time

**Files:** `src/recorder.rs`, `src/app.rs`.

**Contract:** `Recorder::stop(&mut self) -> Result<CaptureReport>`. An `Err` means shutdown failed. An `Ok(report)` means the producer stopped; report flags can still describe corrupt capture. The worker's final `AudioSource::integrity_result()` remains authoritative for capture-error text and recognition suppression.

- [ ] **Step 1: Add a red `NoopRecorder` queue-overflow test.** Start the recorder; fill its five-second queue through its test-visible writer. Expect `stop().unwrap().dropped_samples > 0`. Current `stop()` returns an error.

```rust
let mut recorder = NoopRecorder::default();
recorder.start().unwrap();
recorder.writer.as_mut().unwrap().accept_interleaved(
    &vec![0.25_f32; NOOP_SAMPLE_RATE as usize * (CAPTURE_QUEUE_SECONDS + 1)],
);
let report = recorder.stop().unwrap();
assert!(report.dropped_samples > 0);
assert!(!report.backend_failed);
```

- [ ] **Step 2: Extend daemon capture-failure assertions.** Queue loss, backend failure, and duration overflow must keep `stop_failed == false`. A real stop error must set it. Keep no history or delivery for those failures.
- [ ] **Step 3: Extend the channel-controlled `stop_during_callback_checks_stable_failure_before_final_decode_and_recovers` test.** After its `finish_rx` signal, read `now_ms()` before releasing `audio_resume_tx`. Wait for the clock to advance at least 10 ms with a bounded deadline, then release the worker. Assert stored `stopped_at_ms` is not later than the time read before release. This checks order, not a performance target.
- [ ] **Step 4: Run red tests:** `mise exec -- cargo test --lib recorder::tests` and `mise exec -- cargo test --lib app::tests`. The new contract must fail until implemented.
- [ ] **Step 5: Make `CaptureReport` public because `Recorder` is public.** Add a small `failed()` predicate. Reuse one atomic snapshot helper for `AudioSource::capture_report()` and both concrete `stop()` methods.

```rust
impl CaptureReport {
    pub(crate) fn failed(&self) -> bool {
        self.dropped_samples != 0 || self.backend_failed || self.duration_exceeded
    }
}
impl CaptureIntegrity {
    fn report(&self) -> CaptureReport {
        CaptureReport {
            dropped_samples: self.dropped_samples.load(Ordering::SeqCst),
            backend_failed: self.backend_failed.load(Ordering::SeqCst),
            duration_exceeded: self.duration_exceeded.load(Ordering::SeqCst),
        }
    }
}
// AudioSource::capture_report now returns self.integrity.report().
```
- [ ] **Step 6: Shut down the producer before taking its report.** Preserve invalid-state errors. Keep the worker's independent integrity check after draining. Do not treat report flags as a shutdown error.

```rust
// NoopRecorder::stop, after its existing active-state check:
self.active = false;
self.writer.take();
Ok(self.integrity.as_ref().expect("active capture integrity").report())

// CpalRecorder::stop, after the existing `let stream = self.stream.take()...`:
drop(stream);
Ok(self.integrity.as_ref().expect("active capture integrity").report())
```

- [ ] **Step 7: In the daemon, read `now_ms()` immediately after `recorder.stop()`, before `finish_recognition_worker`.** Use the report to request capture-only worker completion. Set `stop_failed` only for `Err`. Keep `drain_failed` tied to worker drain failure. Move `worker.audio` into `RecordingInput` once; do not clone it.

```rust
let stop = self.recorder.stop();
let stopped_at_ms = now_ms();
let stop_failed = stop.is_err();
let capture_only = stop_failed || stop.as_ref().is_ok_and(CaptureReport::failed);
let worker = self.finish_recognition_worker(capture_only);
let stop_error = stop.err();
// Use stopped_at_ms in RecordingInput after handling the worker result.
```
- [ ] **Step 8: Update all in-repo `Recorder` test implementations.** Let `TestCapture` expose its report to `FixtureRecorder`. Keep `ThreadedRecorder` returning a default report so its worker test still verifies independent integrity checks. The explicit stop-error fixture must return `Err`.
- [ ] **Step 9: Run `mise exec -- cargo test --lib recorder::tests`, `mise exec -- cargo test --lib app::tests`, then `mise exec -- cargo test --quiet`.** Check panic, first-callback, failed-start, cancel, error-precedence, and no-speech cases.
- [ ] **Step 10: Commit:** `git add src/recorder.rs src/app.rs && git commit -m "fix: distinguish recorder shutdown from capture failure"`.

### Task 4: Characterize speech-gate boundaries and document paired replay

**Files:** `src/speech_gate.rs`, `README.md`.

- [ ] **Step 1: Add one characterization test combining `GateInput` and `GateState`.** Use generated samples. This test should pass without changing gate policy.

```rust
#[test]
fn trailing_partial_boundary_is_emitted_once() {
    let mut input = GateInput::new();
    let mut state = GateState::default();
    let frames = input.accept_16khz(&vec![0.5; VAD_FRAME_SAMPLES + 13]);
    assert_eq!(frames.len(), 1);
    assert_eq!(state.accept_frame(&frames[0], true), frame(0.5));
    let tail = input.finish_session();
    assert_eq!(tail.len(), 1);
    assert_eq!(&tail[0][..13], &[0.5; 13]);
    assert!(tail[0][13..].iter().all(|sample| *sample == 0.0));
    assert_eq!(state.accept_frame(&tail[0], false), tail[0]);
    assert!(state.accept_frame(&frame(0.0), false).is_empty());
}
```
- [ ] **Step 2: Run `mise exec -- cargo test --lib speech_gate::tests`.** If the test fails, diagnose the boundary before changing code. Do not use private recordings as test fixtures.
- [ ] **Step 3: Add a short paired-evaluation procedure beside README's existing corpus instructions.** Use one private corrected manifest and identical model/profile values. Document these two commands. Set `umask 077`; place the manifest and output paths in a private directory under `$XDG_STATE_HOME`. Do not run them without the user's consent.

```bash
umask 077
NVSTT_PRIVATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/nvstt/private-evaluation"
mkdir -p "$NVSTT_PRIVATE_DIR"
chmod 700 "$NVSTT_PRIVATE_DIR"
NVSTT_PRIVATE_MANIFEST="$NVSTT_PRIVATE_DIR/manifest.jsonl"
test -f "$NVSTT_PRIVATE_MANIFEST" || { printf 'Add corrected references first.\n' >&2; exit 1; }
nvstt model evaluate --manifest "$NVSTT_PRIVATE_MANIFEST" --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --speech-gate true --json > "$NVSTT_PRIVATE_DIR/gate-on.json"
nvstt model evaluate --manifest "$NVSTT_PRIVATE_MANIFEST" --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --speech-gate false --json > "$NVSTT_PRIVATE_DIR/gate-off.json"
```
- [ ] **Step 4: Compare per-clip reference/hypothesis, silence failures, and edit counts.** Count first/last-word errors by reviewing corrected references. The evaluator does not compute that metric. Do not infer live callback or insertion behavior from replay.
- [ ] **Step 5: Keep the current gate default and model.** If no consented private corpus exists, document the procedure but mark paired accuracy evaluation as pending. Do not claim improved recognition.
- [ ] **Step 6: Commit:** `git add src/speech_gate.rs README.md && git commit -m "test: cover gate tail and document paired evaluation"`.

### Task 5: Align decision records and quality checks

**Files:** `CONTEXT.md`, `docs/adr/0002-persist-bounded-text-history.md`, new `docs/adr/0014-retain-recent-dictation-audio.md`, `src/dictation_transcript.rs`, `src/app.rs` for an ignored memory fixture, plus formatting-only changes in `src/recorder.rs` and `src/speech_gate.rs`.

- [ ] **Step 1: Update the canonical glossary.** Keep the existing `History record` definition. Replace the false audio sentence in `History retention` with: `Recent dictation audio uses a separate seven-entry store.` Add `Recent dictation audio`: `Private original-rate WAV and metadata for a stopped attempt. The store retains up to seven, including failed and no-speech attempts. A save can warn or fail. Cancellation creates no entry.` Delivery can fail after history append.
- [ ] **Step 2: Append a supersession note to ADR 0002.** Preserve its original ten-text-record rationale and history durability policy. Point to ADR 0014 for the newer, separate audio decision.

```markdown
## Subsequent decision

ADR 0014 supersedes only the no-default-audio sentence above. Text history
still keeps ten successful transcripts. Audio retention is a separate store.
```

- [ ] **Step 3: Write ADR 0014.** Record the seven-entry retention, privacy, cancel exclusion, pre-delivery saving, warnings, and no default gate/model change. Link the approved design and the old ADR.

```markdown
# Retain recent stopped dictation audio

Save up to seven stopped attempts as private original-rate WAVs with metadata.
Include success, no speech, and failure. Exclude cancel and failed start.
Save before delivery. A storage warning does not change transcription or delivery.
Keep ten text-history records under ADR 0002. This decision replaces only its
no-default-audio rule. Do not change the default model or speech gate.

Design: docs/superpowers/specs/2026-09-25-dictation-reliability-architecture-design.md.
```

- [ ] **Step 4: Replace the two complex test-table annotations in `src/dictation_transcript.rs` with one named test-local type alias.** Keep the table data and behavior unchanged. Check strict Clippy; use a named test struct only if Clippy still flags the alias.

```rust
type ReplacementCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
```

In both existing tables, replace only `&[(&str, &[(&str, &str)], &str)]` with `&[ReplacementCase<'_>]`. Preserve all case values.
- [ ] **Step 5: Move the ignored benchmark's test-only `SyntheticRecorder` to `app::tests` module scope.** Give its `stop()` the new `Result<CaptureReport>` signature. Add one ignored, single-session memory fixture. Run it alone under `/usr/bin/time -v`. This does not measure model or live microphone RSS.

```rust
#[test]
#[ignore]
fn measure_two_minute_capture_peak_memory() {
    let samples = Arc::new(vec![0.25_f32; 48_000 * 120]);
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recorder = Box::new(SyntheticRecorder { samples, source: None });
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("expected command");
    };
    assert!(result.ok);
    assert_eq!(effects.lock().unwrap().sent.len(), 1);
    assert_eq!(saved_recordings(&directory).len(), 1);
}
```
- [ ] **Step 6: Run `mise exec -- cargo fmt --all`.** Inspect the diff. Keep unrelated changes in the three known baseline-drift files strictly formatting-only.
- [ ] **Step 7: Run final gates:**

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test --quiet
git diff --check
```

- [ ] **Step 8: Commit the checked changes.** Stage only the named files; do not stage private audio, reports, or the untracked research notes.

## Release checks and review gate

- [ ] Build a release test executable without installing it. Run the existing 20 saved and 20 blocked-store trials on the persistent filesystem. Record p50, p95, saved-minus-control differences, and filesystem type. The control still attempts a failed save. The benchmark signals readiness after its first recognizer chunk; worker tail work may remain.
- [ ] Run the single-session 48 kHz, 120-second memory fixture in its own process. Record `/usr/bin/time` maximum resident set size. That value includes the test binary and its allocations, not a live model or microphone. Use the same private run directory:

```bash
set -euo pipefail
umask 077
state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/nvstt"
mkdir -p "$state_dir"
run_dir="$(mktemp -d "$state_dir/reliability-check.XXXXXX")"
test_binary="$(mise exec -- cargo test --release --lib --no-run 2>&1 | awk -F '[()]' '/Executable unittests src\/lib.rs/ { print $2 }')"
test -x "$test_binary"
findmnt -T "$run_dir" -no FSTYPE > "$run_dir/filesystem.txt"
TMPDIR="$run_dir" "$test_binary" --ignored --exact app::tests::measure_two_minute_stop_to_result_with_and_without_storage --nocapture > "$run_dir/latency.txt" 2>&1
/usr/bin/time -v -o "$run_dir/peak-rss.txt" env TMPDIR="$run_dir" "$test_binary" --ignored --exact app::tests::measure_two_minute_capture_peak_memory --nocapture > "$run_dir/memory.txt" 2>&1
```

- [ ] Before any long stress fixture, calculate raw `f32` payload as `4 * rate_hz * 1800` bytes. For each supported input-rate range, use its maximum rate; this bounds every rate in that range. Enumerate this machine's CPAL input ranges only with user consent. A hypothetical 192 kHz payload is 1,382,400,000 bytes, about 1.29 GiB, before spare capacity, queue, and model memory. Do not claim device support or measured RSS from this arithmetic.
- [ ] If saved stop-to-result p95 exceeds the approved 250 ms review threshold in the persistent-filesystem check, stop and ask before changing save order or spooling.
- [ ] Request independent spec and code reviews. Verify the changed paths and all test results after fixes. Keep any unresolved timing or corpus limits explicit.
- [ ] Do not install or restart the linked binary. Ask the user before any live microphone or private-corpus test.
