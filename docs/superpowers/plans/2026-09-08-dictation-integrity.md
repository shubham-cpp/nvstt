# Dictation integrity implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair known audio loss, gate replay, and destructive token cleanup without changing the running dictation setup.

**Architecture:** Keep the existing recorder, processing worker, gate, recognizer, and delivery boundaries. Give the worker a single owned audio consumer and make capture failure prevent transcript delivery. Keep text changes deterministic and local.

**Tech Stack:** Rust 2024, CPAL 0.18.1, rtrb 0.4.0, sherpa-onnx 1.13.4, text-processing-rs 0.2.2, existing unit-test modules.

**Spec:** `docs/superpowers/specs/2026-09-08-dictation-integrity-design.md`, revised in commit `7daf0bb`.

## Global Constraints

- This phase must preserve the final-only delivery contract. It must not record personal audio, install models, change user configuration, or restart the installed daemon.
- Preserve those module boundaries. Do not change the public CLI, IPC schema, recognition trait, model registry, or history format.
- The initial queue capacity is five seconds of mono input at the device sample rate. This value is provisional, not a measured safe capacity or latency bound.
- Keep the existing 30-minute session limit separate.
- Keep current detector thresholds, 400 ms pre-roll capacity, and 200 ms silence bridge. Do not change the 30-second detector setting during this repair.
- Do not add a grammar model, fuzzy correction, vocabulary learning, or paraphrasing. Keep the ITN default unchanged.
- All build, test, and shell commands run inside the `Fedora` distrobox.
- Do not install the repaired binary automatically. Present the test results and diff before any change to the user's active daemon.
- Never use `rm`. Use `gio trash` if a temporary file needs removal.
- Do not stage or modify the existing untracked `HANDOFF.md` or research reports as part of these tasks.

---

## Execution environment and evidence

Start execution with the `using-git-worktrees` skill. Establish an isolated worktree before application edits. Enter Fedora from that worktree:

```bash
distrobox enter Fedora
```

All later Bash blocks run **inside that Fedora shell, at the selected worktree root**. In automated tool calls, wrap the block with `distrobox enter Fedora -- bash -lc` and set its working directory to that same worktree. Do not accidentally build or commit in the original checkout.

Planning baseline: `cargo test --locked --offline --quiet` passed 76 tests on 2026-09-08. No application code changed during planning. The implementation code blocks below have not been compiled as a complete repair; execution must verify them through the stated red/green cycles.

The graph generation `2026-09-04T04:02:44Z` is stale. Current source was read for this plan. Recorded coverage has no parse gaps, but several files changed and the index omits `audio_pipeline.rs`. Do not trust generic graph `drain` or `join` edges. Recheck current source when execution starts.

Queue evidence, retrieved through Fedora during planning:

- `cargo info rtrb@0.4.0` reports MIT/Apache-2.0 and Rust 1.38 minimum.
- [Versioned source](https://docs.rs/crate/rtrb/0.4.0/source/src/lib.rs) documents fixed allocation, immediate push/pop, and full-buffer errors without overwrite.
- `RingBuffer::new(capacity)` returns `Producer<T>` and `Consumer<T>`.
- `Producer::push(&mut self, T)` returns `Result<(), PushError<T>>`.
- `Consumer::slots(&self) -> usize` loads the producer tail with Acquire ordering.
- `Consumer::pop(&mut self) -> Result<T, PopError>` requires exclusive consumer access.
- Producer and consumer are movable between threads when `T: Send`. They are not cloneable endpoints.
- CPAL's cached ALSA `Stream::drop`, lines 1379-1388, joins its callback worker. Retain that Linux shutdown boundary. Do not claim the same contract for unreviewed backends.

An isolated normalizer probe used the existing compiled dependency, not a model. These inputs were unchanged: `5 mm`, `ER diagram`, `a + b = c`, `C++ C# .env config.rs`, `ER ER ER`, `C C++ C`, and `very very`. `DOT` became `.` and `twelve` became `12`. These are planning checks, not ASR benchmarks.

## File map and task order

| File | Responsibility and planned change |
|---|---|
| `src/recorder.rs` | Queue ownership, callback conversion, capture integrity, session lifecycle, test-only capture controls |
| `src/app.rs` | Mutable consumer integration, failure precedence, finalization, counting integration tests |
| `src/speech_gate.rs` | Clear emitted pre-roll and test sample uniqueness |
| `src/dictation_transcript.rs` | Token identity, conservative fillers/stutters, punctuation-safe replacements, normalization order |
| `src/config.rs` | Replacement configuration round-trip test; no config schema change |
| `Cargo.toml`, `Cargo.lock` | Add only the pinned queue dependency |
| `README.md` | User-visible capture failure and replacement semantics |
| `docs/adr/0013-dictation-integrity.md` | Record this repair and supersede the affected parts of ADRs 0011 and 0012 |

No new production module is needed. Keep capture fixtures under `#[cfg(test)]`. Do not split `app.rs` or refactor unrelated code.

Execute Tasks 1 and 2 in order. Task 3 is independent. Tasks 4 and 5 run in order. Task 6 verifies the combined result. Do not run tasks that edit the same file concurrently.

## Task 1: Replace shared capture storage with an owned bounded queue

**Files:** Modify `Cargo.toml`, `Cargo.lock`, `src/recorder.rs`, and the audio-source signatures in `src/app.rs`. Tests remain in `src/recorder.rs::tests`.

**Interfaces:**

- Keep `Recorder::start`, `stop`, and `cancel` returning `Result<()>`.
- Change `Recorder::audio_source(&mut self) -> Result<AudioSource>` to take the sole consumer once.
- `AudioSource::sample_rate(&self) -> i32` remains.
- Change `AudioSource::drain(&mut self) -> Result<Vec<f32>>` to a finite snapshot drain.
- Replace `AudioSource::overflowed` with `AudioSource::integrity_result(&self) -> Result<()>`.
- Add private `CaptureWriter`, `CaptureIntegrity`, and `capture_pair` below. Task 2 uses only the test interface defined here.

- [ ] **Step 1: Capture the baseline and add a failing ownership test.**

```bash
cargo test --locked --offline
```

Add to `recorder::tests`:

```rust
#[test]
fn a_session_has_only_one_consumer() {
    let mut recorder = NoopRecorder::default();
    recorder.start().unwrap();
    let _source = recorder.audio_source().unwrap();
    assert!(recorder.audio_source().is_err());
}
```

```bash
cargo test --locked --offline recorder::tests::a_session_has_only_one_consumer
```

Expected before repair: assertion failure because the second call succeeds. Do not proceed if the baseline fails for unrelated reasons.

- [ ] **Step 2: Add the queue dependency and the capture regression tests.**

Add under `[dependencies]`:

```toml
rtrb = "=0.4.0"
```

Resolve only this addition with `cargo check --offline`. If the crate is absent from the executor's cache, run `cargo info rtrb@0.4.0` inside Fedora first. Review the lockfile diff; do not update unrelated versions.

Add these tests before the new implementation. A missing new type is initially expected; after introducing the types, verify that the assertions discriminate the behavior.

```rust
#[test]
fn queue_loss_is_counted_without_overwriting_audio() {
    let (mut writer, mut source) = capture_pair(16_000, 1, 2, 100);
    writer.accept_interleaved(&[1.0_f32, 2.0, 3.0]);
    assert_eq!(source.drain().unwrap(), vec![1.0, 2.0]);
    let message = source.integrity_result().unwrap_err().to_string();
    assert!(message.contains("1 mono samples dropped"));
}

#[test]
fn duration_counts_input_that_did_not_fit_the_queue() {
    let (mut writer, source) = capture_pair(16_000, 1, 1, 2);
    writer.accept_interleaved(&[1.0_f32, 2.0, 3.0]);
    let message = source.integrity_result().unwrap_err().to_string();
    assert!(message.contains("1 mono samples dropped"));
    assert!(message.contains("30 minute limit"));
    assert_eq!(writer.total_samples, 3);
}

#[test]
fn backend_failure_survives_an_unavailable_diagnostic_slot() {
    let (_writer, source) = capture_pair(16_000, 1, 2, 100);
    let guard = source.integrity.backend_message.lock().unwrap();
    source.integrity.record_backend_error(&"injected device failure");
    drop(guard);
    let message = source.integrity_result().unwrap_err().to_string();
    assert!(message.contains("backend capture error"));
    assert!(!message.contains("mono samples dropped"));
}

#[test]
fn old_consumer_cannot_read_the_next_session() {
    let mut recorder = NoopRecorder::default();
    recorder.start().unwrap();
    let mut old = recorder.audio_source().unwrap();
    recorder.cancel().unwrap();
    recorder.start().unwrap();
    let mut new = recorder.audio_source().unwrap();
    assert!(old.drain().unwrap().is_empty());
    assert!(new.drain().unwrap().is_empty());
    assert!(!Arc::ptr_eq(&old.integrity, &new.integrity));
}
```

```bash
cargo test --offline recorder::tests
```

Expected at this point: missing `capture_pair` or integrity fields. Keep the already-observed behavioral failure from Step 1 as the ownership regression. Introduce the types in Step 3, then rerun this command during implementation to observe and fix the loss/accounting assertions before the final full-suite run.

- [ ] **Step 3: Implement the private queue and failure state.**

Replace `CaptureState` and the mutex-backed source with this core. Add imports for `std::sync::atomic::{AtomicBool, AtomicUsize, Ordering}`, `std::fmt::Display`, and `rtrb::{Consumer, Producer, RingBuffer}`. Keep `Arc` and `Mutex` for failure metadata only. Remove `VecDeque` and `tracing::warn` when no longer used.

```rust
const CAPTURE_QUEUE_SECONDS: usize = 5;

#[derive(Debug, Default)]
struct CaptureIntegrity {
    dropped_samples: AtomicUsize,
    duration_exceeded: AtomicBool,
    backend_failed: AtomicBool,
    backend_message: Mutex<Option<String>>,
}

impl CaptureIntegrity {
    fn record_backend_error(&self, error: &impl Display) {
        self.backend_failed.store(true, Ordering::SeqCst);
        if let Ok(mut message) = self.backend_message.try_lock() {
            *message = Some(error.to_string());
        }
    }

    fn result(&self) -> Result<()> {
        let mut reasons = Vec::new();
        let dropped = self.dropped_samples.load(Ordering::SeqCst);
        if dropped != 0 {
            reasons.push(format!("{dropped} mono samples dropped by the capture queue"));
        }
        if self.backend_failed.load(Ordering::SeqCst) {
            let detail = self.backend_message.try_lock().ok()
                .and_then(|message| message.clone());
            reasons.push(match detail {
                Some(detail) => format!("backend capture error: {detail}"),
                None => "backend capture error".to_owned(),
            });
        }
        if self.duration_exceeded.load(Ordering::SeqCst) {
            reasons.push("audio capture exceeded the 30 minute limit".to_owned());
        }
        if reasons.is_empty() {
            Ok(())
        } else {
            Err(AppError::Unavailable(format!(
                "audio capture integrity failure: {}", reasons.join("; ")
            )))
        }
    }
}

#[derive(Debug)]
struct CaptureWriter {
    producer: Producer<f32>,
    integrity: Arc<CaptureIntegrity>,
    channels: usize,
    total_samples: usize,
    max_samples: usize,
}

impl CaptureWriter {
    fn accept_interleaved<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: cpal::FromSample<T>,
    {
        for frame in data.chunks(self.channels) {
            let beyond_limit = self.total_samples >= self.max_samples;
            self.total_samples = self.total_samples.saturating_add(1);
            if beyond_limit {
                self.integrity.duration_exceeded.store(true, Ordering::SeqCst);
                continue;
            }
            let sum = frame.iter().map(|sample| f32::from_sample(*sample)).sum::<f32>();
            if self.producer.push(sum / frame.len() as f32).is_err() {
                self.integrity.dropped_samples.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

#[derive(Debug)]
pub struct AudioSource {
    consumer: Consumer<f32>,
    sample_rate: i32,
    integrity: Arc<CaptureIntegrity>,
}

fn capture_pair(
    sample_rate: i32,
    channels: usize,
    capacity: usize,
    max_samples: usize,
) -> (CaptureWriter, AudioSource) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let integrity = Arc::new(CaptureIntegrity::default());
    (
        CaptureWriter {
            producer,
            integrity: Arc::clone(&integrity),
            channels,
            total_samples: 0,
            max_samples,
        },
        AudioSource { consumer, sample_rate, integrity },
    )
}

impl AudioSource {
    pub(crate) fn sample_rate(&self) -> i32 { self.sample_rate }

    pub(crate) fn drain(&mut self) -> Result<Vec<f32>> {
        let available = self.consumer.slots();
        self.drain_snapshot(available)
    }

    fn drain_snapshot(&mut self, available: usize) -> Result<Vec<f32>> {
        let mut samples = Vec::with_capacity(available);
        for _ in 0..available {
            samples.push(self.consumer.pop().map_err(|_| {
                AppError::Unavailable("capture queue snapshot became unreadable".to_owned())
            })?);
        }
        Ok(samples)
    }

    pub(crate) fn integrity_result(&self) -> Result<()> { self.integrity.result() }

    #[cfg(test)]
    pub(crate) fn test_source(sample_rate: i32, samples: Vec<f32>) -> Self {
        let (mut writer, source) = capture_pair(
            sample_rate, 1, samples.len().max(1), usize::MAX,
        );
        writer.accept_interleaved(&samples);
        source
    }
}
```

`capture_pair` is private and receives validated rates/channels from `CpalRecorder::start`; test callers use positive values. Do not add arbitrary public configuration for test capacities. The snapshot-pop failure is an internal invariant guard, not an expected empty-buffer event.

- [ ] **Step 4: Wire both recorders and preserve all sample formats.**

Use these fields and the one-time acquisition helper:

```rust
#[derive(Debug, Default)]
pub struct NoopRecorder {
    active: bool,
    writer: Option<CaptureWriter>,
    source: Option<AudioSource>,
    integrity: Option<Arc<CaptureIntegrity>>,
}

#[derive(Default)]
pub struct CpalRecorder {
    stream: Option<Stream>,
    source: Option<AudioSource>,
    integrity: Option<Arc<CaptureIntegrity>>,
}

fn take_audio_source(source: &mut Option<AudioSource>) -> Result<AudioSource> {
    source.take().ok_or_else(|| AppError::InvalidState(
        "capture consumer is unavailable or already acquired".to_owned()
    ))
}
```

Change the trait and both implementations to `audio_source(&mut self)`. Call `take_audio_source(&mut self.source)`. Delete `live_audio_source` and `append_interleaved` after replacing their callers.

For `NoopRecorder::start`, retain the active-state guard, then create and store the pair:

```rust
let (writer, source) = capture_pair(
    NOOP_SAMPLE_RATE, 1,
    NOOP_SAMPLE_RATE as usize * CAPTURE_QUEUE_SECONDS,
    NOOP_SAMPLE_RATE as usize * MAX_CAPTURE_SECONDS,
);
self.integrity = Some(Arc::clone(&source.integrity));
self.writer = Some(writer);
self.source = Some(source);
self.active = true;
```

For `NoopRecorder::stop`, retain the inactive-state error, set `active = false`, drop `self.writer.take()`, and return the retained integrity result. `cancel` drops writer first, then source and integrity, and clears active state. Do not clear a moved consumer's state through its `Arc`.

Replace `CpalRecorder::build_stream` with:

```rust
fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut writer: CaptureWriter,
) -> std::result::Result<Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let integrity = Arc::clone(&writer.integrity);
    let error_callback = move |error: cpal::Error| {
        integrity.record_backend_error(&error);
    };
    let data_callback = move |data: &[T], _info: &cpal::InputCallbackInfo| {
        writer.accept_interleaved(data);
    };
    device.build_input_stream(config, data_callback, error_callback, None)
        .map_err(|error| error.to_string())
}
```

After the existing channel/rate validation in `start`, construct the pair and keep an integrity handle. Replace the format match with:

```rust
let (writer, source) = capture_pair(
    sample_rate, channels,
    sample_rate as usize * CAPTURE_QUEUE_SECONDS,
    sample_rate as usize * MAX_CAPTURE_SECONDS,
);
let integrity = Arc::clone(&source.integrity);
let sample_format = supported.sample_format();
let config: StreamConfig = supported.into();
let stream = match sample_format {
    SampleFormat::F32 => Self::build_stream::<f32>(&device, config, writer),
    SampleFormat::I8 => Self::build_stream::<i8>(&device, config, writer),
    SampleFormat::I16 => Self::build_stream::<i16>(&device, config, writer),
    SampleFormat::I24 => Self::build_stream::<cpal::I24>(&device, config, writer),
    SampleFormat::I32 => Self::build_stream::<i32>(&device, config, writer),
    SampleFormat::I64 => Self::build_stream::<i64>(&device, config, writer),
    SampleFormat::U8 => Self::build_stream::<u8>(&device, config, writer),
    SampleFormat::U16 => Self::build_stream::<u16>(&device, config, writer),
    SampleFormat::U24 => Self::build_stream::<cpal::U24>(&device, config, writer),
    SampleFormat::U32 => Self::build_stream::<u32>(&device, config, writer),
    SampleFormat::U64 => Self::build_stream::<u64>(&device, config, writer),
    SampleFormat::F64 => Self::build_stream::<f64>(&device, config, writer),
    format => return Err(AppError::Unavailable(format!(
        "unsupported audio sample format: {format}"
    ))),
}.map_err(|error| AppError::Unavailable(format!(
    "could not build audio input stream: {error}"
)))?;
```

Keep the existing `stream.play()` error handling. Only after play succeeds, store stream, source, and integrity. Locals drop safely on build/play failure.

Use these `CpalRecorder` methods to retain shutdown ordering and the existing `Result<()>` shape:

```rust
fn stop(&mut self) -> Result<()> {
    let stream = self.stream.take().ok_or_else(|| {
        AppError::InvalidState("recorder is not active".to_owned())
    })?;
    drop(stream);
    self.integrity.as_ref().expect("active capture integrity").result()
}

fn cancel(&mut self) -> Result<()> {
    self.stream.take();
    self.source.take();
    self.integrity.take();
    Ok(())
}

fn audio_source(&mut self) -> Result<AudioSource> {
    take_audio_source(&mut self.source)
}
```

For `NoopRecorder`, the corresponding stop/cancel methods are:

```rust
fn stop(&mut self) -> Result<()> {
    if !self.active {
        return Err(AppError::InvalidState("recorder is not active".to_owned()));
    }
    self.active = false;
    self.writer.take();
    self.integrity.as_ref().expect("active capture integrity").result()
}

fn cancel(&mut self) -> Result<()> {
    self.writer.take();
    self.source.take();
    self.integrity.take();
    self.active = false;
    Ok(())
}

fn audio_source(&mut self) -> Result<AudioSource> {
    take_audio_source(&mut self.source)
}
```

- [ ] **Step 5: Adapt worker borrowing and add deterministic queue tests.**

In `src/app.rs`, make the owned `source` in `run_recognition_worker` mutable. Change the `source` parameter to `&mut AudioSource` in `feed_audio_if_healthy`, `finish_audio_if_healthy`, `feed_available_audio`, `drain_audio`, and `finish_audio`. Pass `&mut source` from the worker. Replace the old `overflowed` block at the end of `finish_audio` with `source.integrity_result()?` for now. Task 2 moves the decisive final check ahead of decoding.

Update the old downmix test to use `capture_pair` and `writer.accept_interleaved`. Make local sources mutable in existing recorder tests.

Add these deterministic tests:

```rust
#[test]
fn concurrent_producer_preserves_order_without_exceeding_capacity() {
    use std::sync::Barrier;
    let (mut writer, mut source) = capture_pair(16_000, 1, 64, 1_000);
    let barrier = Arc::new(Barrier::new(2));
    let producer_barrier = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || {
        for batch in 0..4 {
            let values = (batch * 64..(batch + 1) * 64)
                .map(|n| n as f32).collect::<Vec<_>>();
            writer.accept_interleaved(&values);
            producer_barrier.wait();
            producer_barrier.wait();
        }
    });
    let mut actual = Vec::new();
    for _ in 0..4 {
        barrier.wait();
        actual.extend(source.drain().unwrap());
        barrier.wait();
    }
    handle.join().unwrap();
    assert_eq!(actual, (0..256).map(|n| n as f32).collect::<Vec<_>>());
    source.integrity_result().unwrap();
}

#[test]
fn final_sample_and_loss_are_visible_after_producer_join() {
    use std::sync::Barrier;
    let (mut writer, mut source) = capture_pair(16_000, 1, 1, 100);
    let barrier = Arc::new(Barrier::new(2));
    let producer_barrier = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || {
        producer_barrier.wait();
        writer.accept_interleaved(&[7.0_f32, 8.0]);
    });
    barrier.wait();
    handle.join().unwrap();
    assert_eq!(source.drain().unwrap(), vec![7.0]);
    assert!(source.integrity_result().unwrap_err().to_string()
        .contains("1 mono samples dropped"));
}

#[test]
fn entry_snapshot_does_not_expand_with_new_audio() {
    let (mut writer, mut source) = capture_pair(16_000, 1, 4, 100);
    writer.accept_interleaved(&[1.0_f32, 2.0]);
    let snapshot = source.consumer.slots();
    writer.accept_interleaved(&[3.0_f32, 4.0]);
    assert_eq!(source.drain_snapshot(snapshot).unwrap(), vec![1.0, 2.0]);
    assert_eq!(source.drain().unwrap(), vec![3.0, 4.0]);
}
```

The last test calls the same snapshot-drain helper used in production, after deterministic publication of more audio. It does not duplicate the algorithm in the test. Also inspect `AudioSource::drain` to ensure it captures the bound once, not `while pop().is_ok()`.

- [ ] **Step 6: Verify and commit the queue deliverable.**

Before the full-suite run, verify the new assertions discriminate the failure paths: temporarily suppress the queue-loss increment, run `queue_loss_is_counted_without_overwriting_audio`, and confirm failure. Restore it. Temporarily suppress the backend flag update and confirm `backend_failure_survives_an_unavailable_diagnostic_slot` fails, then restore it. Do not commit either temporary mutation.

```bash
cargo test --locked --offline recorder::tests
cargo test --locked --offline app::tests::worker_drains_the_last_audio_before_final_flush
cargo test --locked --offline
git diff --check
git add Cargo.toml Cargo.lock src/recorder.rs src/app.rs
git commit -m "fix: preserve capture audio with an owned bounded queue"
```

Expected: all existing tests and new recorder tests pass. Review callback code for allocation, mutex access, retry loops, and accidental removal of sample formats. No inference or microphone test is needed.

## Task 2: Prove capture failures cannot reach history or delivery

**Files:** Modify `src/recorder.rs` test support and `src/app.rs` worker finalization/tests.

**Interfaces:** Consume Task 1's `AudioSource::integrity_result`. Add test-only `TestCapture::new()`, `push`, `backend_error`, `close`, and `take_source` below. Production recorder interfaces remain unchanged from Task 1.

- [ ] **Step 1: Add test-only control and counting fixtures.**

Put this under `#[cfg(test)]` in `recorder.rs`:

```rust
pub(crate) struct TestCapture {
    writer: Option<CaptureWriter>,
    source: Option<AudioSource>,
    integrity: Arc<CaptureIntegrity>,
}

impl TestCapture {
    pub(crate) fn new() -> Self {
        let (writer, source) = capture_pair(16_000, 1, 2, 100);
        Self {
            integrity: Arc::clone(&source.integrity),
            writer: Some(writer),
            source: Some(source),
        }
    }
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.writer.as_mut().unwrap().accept_interleaved(samples);
    }
    pub(crate) fn backend_error(&self) {
        self.integrity.record_backend_error(&"injected backend failure");
    }
    pub(crate) fn close(&mut self) { self.writer.take(); }
    pub(crate) fn take_source(&mut self) -> AudioSource { self.source.take().unwrap() }
}
```

Use only test-local mutexes for these app fixtures. Add in `app::tests`:

```rust
use std::sync::{Arc, Mutex};
use crate::recorder::TestCapture;

#[derive(Default)]
struct Effects {
    sent: Vec<String>,
    records: Vec<HistoryRecord>,
    delivery_updates: usize,
}

struct CountingSink(Arc<Mutex<Effects>>);
impl TextSink for CountingSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        self.0.lock().unwrap().sent.push(text.to_owned());
        Ok(DeliveryOutcome::Delivered { backend: "test".to_owned() })
    }
}

struct CountingHistory(Arc<Mutex<Effects>>);
impl HistoryStore for CountingHistory {
    fn append(&mut self, record: HistoryRecord) -> Result<()> {
        self.0.lock().unwrap().records.push(record);
        Ok(())
    }
    fn update_delivery(
        &mut self, _id: &str, _status: DeliveryStatus, _backend: Option<String>,
    ) -> Result<()> {
        self.0.lock().unwrap().delivery_updates += 1;
        Ok(())
    }
    fn list(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        Ok(self.0.lock().unwrap().records.iter().rev().take(limit).cloned().collect())
    }
}

#[derive(Clone, Copy)]
enum CaptureFault { None, Queue, Backend, Both }

struct FixtureRecorder {
    fault: CaptureFault,
    session: Option<TestCapture>,
}
impl Recorder for FixtureRecorder {
    fn start(&mut self) -> Result<()> {
        let mut session = TestCapture::new();
        match self.fault {
            CaptureFault::None => session.push(&[0.25]),
            CaptureFault::Queue => session.push(&[0.25, 0.5, 0.75]),
            CaptureFault::Backend => session.backend_error(),
            CaptureFault::Both => {
                session.push(&[0.25, 0.5, 0.75]);
                session.backend_error();
            }
        }
        self.fault = CaptureFault::None;
        self.session = Some(session);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.session.as_mut().unwrap().close();
        // Deliberately return Ok: the worker must independently check integrity.
        Ok(())
    }
    fn cancel(&mut self) -> Result<()> {
        if let Some(mut session) = self.session.take() { session.close(); }
        Ok(())
    }
    fn audio_source(&mut self) -> Result<AudioSource> {
        Ok(self.session.as_mut().unwrap().take_source())
    }
}

fn observed_daemon(fault: CaptureFault) -> (Daemon, Arc<Mutex<Effects>>) {
    let effects = Arc::new(Mutex::new(Effects::default()));
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(FixtureRecorder { fault, session: None }),
        Box::new(StaticRecognizer::new("final transcript")),
        Box::new(CountingSink(Arc::clone(&effects))),
        Box::new(CountingHistory(Arc::clone(&effects))),
        Box::new(NoopNotifier::default()),
    );
    daemon.initialize();
    (daemon, effects)
}
```

Fault injection occurs before the worker consumes audio, so queue overflow tests are not scheduler-dependent.

- [ ] **Step 2: Add application assertions and the error-precedence regression.**

```rust
#[test]
fn successful_stop_delivers_once_and_only_after_stop() {
    let (mut daemon, effects) = observed_daemon(CaptureFault::None);
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(effects.lock().unwrap().sent.is_empty());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let effects = effects.lock().unwrap();
    assert_eq!(effects.sent, vec!["final transcript"]);
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.records[0].transcript, "final transcript");
    assert_eq!(effects.delivery_updates, 1);
}

#[test]
fn capture_failures_do_not_deliver_or_append_history_and_recover() {
    for fault in [CaptureFault::Queue, CaptureFault::Backend, CaptureFault::Both] {
        let (mut daemon, effects) = observed_daemon(fault);
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        let response = daemon.handle(IpcRequest::Toggle);
        let IpcResponse::Command { result } = response else { panic!("expected command") };
        assert!(!result.ok);
        assert_eq!(result.transcription, TranscriptionStatus::Failed);
        assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
        assert_eq!(result.status.state, DaemonState::Idle);
        assert!(result.transcript.is_none());
        {
            let effects = effects.lock().unwrap();
            assert!(effects.sent.is_empty());
            assert!(effects.records.is_empty());
            assert_eq!(effects.delivery_updates, 0);
        }
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        assert_eq!(effects.lock().unwrap().sent, vec!["final transcript"]);
    }
}

#[test]
fn cancel_with_a_full_queue_has_no_effects_and_recovers() {
    let (mut daemon, effects) = observed_daemon(CaptureFault::Queue);
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
    assert!(effects.lock().unwrap().sent.is_empty());
    assert!(effects.lock().unwrap().records.is_empty());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(effects.lock().unwrap().sent.len(), 1);
}
```

Also test the recorder-stop error branch, not only the independent worker check:

```rust
struct StopErrorRecorder(FixtureRecorder);
impl Recorder for StopErrorRecorder {
    fn start(&mut self) -> Result<()> { self.0.start() }
    fn stop(&mut self) -> Result<()> {
        self.0.stop()?;
        Err(AppError::Unavailable("injected backend capture error".to_owned()))
    }
    fn cancel(&mut self) -> Result<()> { self.0.cancel() }
    fn audio_source(&mut self) -> Result<AudioSource> { self.0.audio_source() }
}

#[test]
fn recorder_stop_error_never_reaches_history_or_delivery() {
    let (mut daemon, effects) = observed_daemon(CaptureFault::None);
    daemon.recorder = Box::new(StopErrorRecorder(FixtureRecorder {
        fault: CaptureFault::None, session: None,
    }));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle)
        else { panic!("expected command") };
    assert_eq!(result.transcription, TranscriptionStatus::Failed);
    assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
    assert_eq!(result.status.state, DaemonState::Idle);
    assert!(result.transcript.is_none());
    assert!(effects.lock().unwrap().sent.is_empty());
    assert!(effects.lock().unwrap().records.is_empty());
}
```

For worker-error precedence, directly test the new `finish_worker_session` helper specified in Step 3. This avoids inventing timing hooks in the production worker:

```rust
#[test]
fn capture_failure_overrides_prior_worker_error_without_finalizing() {
    struct MustNotFinish;
    impl StreamingRecognizer for MustNotFinish {
        fn start_session(&mut self) -> Result<()> { Ok(()) }
        fn accept_audio(&mut self, _rate: i32, _samples: &[f32]) -> Result<()> {
            panic!("must not decode failed capture")
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            panic!("must not finalize failed capture")
        }
        fn cancel_session(&mut self) -> Result<()> { Ok(()) }
    }
    let mut capture = TestCapture::new();
    capture.push(&[1.0, 2.0, 3.0]);
    capture.backend_error();
    let mut source = capture.take_source();
    capture.close();
    let error = finish_worker_session(
        &mut source, &mut MustNotFinish, &mut AudioPipeline::new(false),
        Some(AppError::Unavailable("earlier recognizer failure".to_owned())),
    ).unwrap_err().to_string();
    assert!(error.contains("mono samples dropped"));
    assert!(error.contains("backend capture error"));
    assert!(!error.contains("earlier recognizer failure"));
}
```

Run:

```bash
cargo test --locked --offline app::tests
```

The new helper test starts with a missing function. Once the helper exists, temporarily use the old error-first selection to confirm the precedence assertion fails. Restore the repaired selection before committing. Do not claim the old static sink proved these properties.

- [ ] **Step 3: Check integrity before final ASR work, independently of worker health.**

Add this helper in `app.rs`:

```rust
fn finish_worker_session(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    worker_error: Option<AppError>,
) -> Result<RecognitionOutcome> {
    if let Err(error) = source.integrity_result() {
        let _ = recognizer.cancel_session();
        return Err(error);
    }
    if let Some(error) = worker_error {
        let _ = recognizer.cancel_session();
        return Err(error);
    }
    finish_audio(source, recognizer, audio_pipeline)?;
    recognizer.finish_session()
}
```

The caller must have stopped/joined the producer before `WorkerCommand::Finish`. `Daemon::finish_listening` already calls `recorder.stop()` before worker finish; preserve that order. Task 1's real recorder returns backend/capture errors directly, which takes the existing `transcription_failure` cancellation path.

Replace the worker's Finish branch with:

```rust
Ok(WorkerCommand::Finish) => {
    break Some(finish_worker_session(
        &mut source,
        recognizer.as_mut(),
        &mut audio_pipeline,
        worker_error.take(),
    ));
}
```

Remove `finish_audio_if_healthy`, now orphaned. Keep the final `source.integrity_result()?` in `finish_audio` as a defensive check; it must not be the only check. Do not move history creation ahead of successful recognition.

- [ ] **Step 4: Test close/cancel with an abandoned consumer, then verify and commit.**

Add to `recorder::tests`:

```rust
#[test]
fn producer_shutdown_does_not_need_a_live_consumer() {
    let (mut writer, source) = capture_pair(16_000, 1, 1, 100);
    writer.accept_interleaved(&[1.0_f32, 2.0]);
    drop(source);
    drop(writer);
}
```

```bash
cargo test --locked --offline app::tests
cargo test --locked --offline recorder::tests
cargo test --locked --offline
git diff --check
git add src/app.rs src/recorder.rs
git commit -m "test: enforce capture failure isolation and recovery"
```

Review the returned recognizer lifecycle and next-session tests. Preserve delivery failure history behavior, which is separate from capture failure.

## Task 3: Emit each speech-gate input sample at most once

**Files:** Modify and test `src/speech_gate.rs` only.

**Interfaces:** Keep `GateState::accept_frame(&mut self, frame: &[f32], detected: bool) -> Vec<f32>` and all public gate/recognizer interfaces unchanged.

- [ ] **Step 1: Add a failing exact-output regression.**

```rust
#[test]
fn restart_never_replays_emitted_frames() {
    let mut state = GateState::default();
    let first = frame(1.0);
    let boundary = frame(2.0);
    let second = frame(3.0);
    assert_eq!(state.accept_frame(&first, true), first);
    assert_eq!(state.accept_frame(&boundary, false), boundary);
    let output = state.accept_frame(&second, true);
    let expected = [vec![0.0; SILENCE_BRIDGE_SAMPLES], second].concat();
    assert_eq!(output, expected);
}

#[test]
fn repeated_transitions_preserve_only_unemitted_preroll() {
    let mut state = GateState::default();
    let mut nonzero = Vec::new();
    for (value, detected) in [
        (1.0, true), (2.0, false), (3.0, false),
        (4.0, true), (5.0, false), (6.0, true),
    ] {
        nonzero.extend(state.accept_frame(&frame(value), detected)
            .into_iter().filter(|sample| *sample != 0.0));
    }
    let expected = (1..=6).flat_map(|value| frame(value as f32)).collect::<Vec<_>>();
    assert_eq!(nonzero, expected);
}
```

```bash
cargo test --locked --offline speech_gate::tests::restart_never_replays_emitted_frames
```

Expected: old code emits previous frames again, so vector equality fails.

- [ ] **Step 2: Clear pre-roll after emitting it or an active frame.**

In `GateState::accept_frame`, retain the current push and match branches. Assign the match result to `output` instead of returning it directly, then finish with:

```rust
if !output.is_empty() {
    self.pre_roll.clear();
}
output
```

The complete control-flow shape is:

```rust
fn accept_frame(&mut self, frame: &[f32], detected: bool) -> Vec<f32> {
    self.push_pre_roll(frame);
    let output = match (self.active, detected) {
        (false, false) => Vec::new(),
        (false, true) => {
            let bridge = if self.saw_speech { SILENCE_BRIDGE_SAMPLES } else { 0 };
            let mut output = Vec::with_capacity(bridge + self.pre_roll.len());
            output.resize(bridge, 0.0);
            output.extend(self.pre_roll.iter().copied());
            self.active = true;
            self.saw_speech = true;
            output
        }
        (true, true) => frame.to_vec(),
        (true, false) => {
            self.active = false;
            frame.to_vec()
        }
    };
    if !output.is_empty() { self.pre_roll.clear(); }
    output
}
```

Retain the existing boundary-frame comment. Do not change detector thresholds, padding, native flush handling, or the ASR stream lifetime.

- [ ] **Step 3: Verify exact bridge content and retained framing tests.**

Tighten `adds_a_short_silence_bridge_between_regions` to assert the whole output equals `[bridge, new_speech]`, using the exact-output construction above. Keep all existing long-pause, reset, arbitrary-chunk, and final-sample tests.

```bash
cargo test --locked --offline speech_gate::tests
cargo test --locked --offline
git diff --check
git add src/speech_gate.rs
git commit -m "fix: prevent speech gate preroll from replaying audio"
```

Acceptance covers synthetic detection decisions only. Do not claim the 25-second symptom is fixed.

## Task 4: Preserve technical tokens through filler and stutter cleanup

**Files:** Modify `src/dictation_transcript.rs` and affected assertions in its tests. Do not change ITN order yet.

**Interfaces:** Add private `TokenParts<'a>` and `token_parts(&str) -> TokenParts<'_>`. Keep `word_core(&str) -> &str` as a wrapper using the same identity. Task 5 consumes `token_parts` for replacements.

- [ ] **Step 1: Add full-cleanup preservation tests.**

```rust
#[test]
fn technical_tokens_survive_cleanup_with_and_without_itn() {
    for itn in [false, true] {
        for text in [
            "5 mm", "ER diagram", "a + b = c", "C++ C# .env config.rs",
            "ER ER ER", "C C++ C", "very very", "UH UM",
        ] {
            assert_eq!(
                dictation_transcript(transcript(text), &Replacements::default(), itn),
                Ok(DictationTranscript::Ready(text.to_owned())),
                "input={text:?}, itn={itn}",
            );
        }
    }
}

#[test]
fn punctuation_only_input_is_not_silently_discarded() {
    assert_eq!(clean("+ = /"), Ok(DictationTranscript::Ready("+ = /".to_owned())));
}
```

```bash
cargo test --locked --offline dictation_transcript::tests::technical_tokens_survive_cleanup_with_and_without_itn
```

Expected: current code removes `mm`, `ER`, and symbols. These assertions are mandatory; do not remove failing rows if ITN differs on another environment.

- [ ] **Step 2: Define one bounded token-identity rule.**

Replace `word_core`/`is_word_char` with:

```rust
#[derive(Clone, Copy, Debug)]
struct TokenParts<'a> {
    leading: &'a str,
    core: &'a str,
    trailing: &'a str,
}

fn token_parts(token: &str) -> TokenParts<'_> {
    let rest = token.trim_start_matches(|c: char| {
        matches!(c, '\"' | '\'' | '“' | '‘' | '(' | '[' | '{')
    });
    let core = rest.trim_end_matches(|c: char| {
        matches!(c, '\"' | '\'' | '”' | '’' | ')' | ']' | '}' | ',' | '.' | '!' | '?' | ';' | ':')
    });
    if core.is_empty() {
        return TokenParts { leading: "", core: token, trailing: "" };
    }
    let start = token.len() - rest.len();
    let end = start + core.len();
    TokenParts { leading: &token[..start], core, trailing: &token[end..] }
}

fn word_core(token: &str) -> &str { token_parts(token).core }
```

The punctuation-only fallback permits explicit rules for `.` and does not turn them into empty patterns. Leading dots and technical suffixes stay in `core`. This policy intentionally does not parse programming languages or filenames ending with a sentence period.

- [ ] **Step 3: Narrow fillers and validate every token in a stutter run.**

Use this filler list and matching body:

```rust
const FILLED_PAUSES: &[&str] = &["uh", "uhh", "uhhh", "um", "umm", "ummm"];

fn is_filled_pause(token: &str) -> bool {
    let core = word_core(token);
    let lowercase = core.bytes().all(|b| b.is_ascii_lowercase());
    let title_case = core.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && core.as_bytes()[1..].iter().all(u8::is_ascii_lowercase);
    (lowercase || title_case)
        && FILLED_PAUSES.iter().any(|filler| core.eq_ignore_ascii_case(filler))
}

fn is_short_stutter_token(core: &str) -> bool {
    (1..=STUTTER_MAX_LETTERS).contains(&core.len())
        && core.bytes().all(|byte| byte.is_ascii_alphabetic())
        && !(core.len() >= 2 && core.bytes().all(|byte| byte.is_ascii_uppercase()))
}
```

In the `collapse_stutters` while condition, require the next token to qualify before comparing its core:

```rust
while index + run < tokens.len()
    && is_short_stutter_token(word_core(tokens[index + run]))
    && word_core(tokens[index + run]).eq_ignore_ascii_case(core)
{
    run += 1;
}
```

In `dictation_transcript`, keep only `!is_filled_pause(token)` in the filter. Remove the empty-core discard.

Rename `match_is_case_insensitive` to `uppercase_acronyms_are_not_fillers` and change its expected output for `UH Hello UM` to `UH Hello UM`. Add a lowercase/title-case assertion:

```rust
assert_eq!(clean("uh Hello Um"), Ok(DictationTranscript::Ready("Hello".to_owned())));
```

Retain the `I I I I think`, intentional-double, `uh-huh`, and filler-only tests. Update module comments to say that ambiguous tokens remain.

- [ ] **Step 4: Verify and commit.**

```bash
cargo test --locked --offline dictation_transcript::tests
cargo test --locked --offline app::tests
cargo test --locked --offline
git diff --check
git add src/dictation_transcript.rs
git commit -m "fix: preserve technical tokens during dictation cleanup"
```

## Task 5: Preserve replacement identity, punctuation, and configured values

**Files:** Modify `src/dictation_transcript.rs`, configuration tests in `src/config.rs`, and app tests in `src/app.rs`.

**Interfaces:** Keep `Replacements::from_pairs`, serde support, and `dictation_transcript` signatures unchanged. Extend private `ReplacementRule` with `original_pattern: String`. Keep `match_at(&self, tokens: &[&str], index: usize) -> Option<(usize, &str)>`.

- [ ] **Step 1: Add the replacement regression table and order test.**

```rust
#[test]
fn replacements_preserve_identity_and_sentence_wrappers() {
    let cases: &[(&str, &[(&str, &str)], &str)] = &[
        ("C C++ C#", &[("C", "cee"), ("C++", "cpp"), ("C#", "csharp")], "cee cpp csharp"),
        ("\"nv stt.\"", &[("nv stt", "nvstt")], "\"nvstt.\""),
        ("nv stt.", &[("nv stt", "nvstt.")], "nvstt."),
        ("nv. stt", &[("nv stt", "nvstt")], "nv. stt"),
        ("nv, stt", &[("nv stt", "nvstt")], "nv, stt"),
        ("keep \"scratch that.\"", &[("scratch that", "")], "keep"),
        (".env config.rs", &[("env", "wrong"), ("config", "wrong")], ".env config.rs"),
        (".", &[(".", "")], ""),
    ];
    for (input, pairs, expected) in cases {
        let expected = if expected.is_empty() {
            DictationTranscript::NoContent
        } else {
            DictationTranscript::Ready((*expected).to_owned())
        };
        assert_eq!(clean_with(input, pairs), Ok(expected), "input={input:?}");
    }
}

#[test]
fn normalized_patterns_match_but_values_are_not_normalized_again() {
    let rules = Replacements::from_pairs([
        ("12".to_owned(), "twenty one".to_owned()),
    ]);
    assert_eq!(
        dictation_transcript(transcript("twelve"), &rules, true),
        Ok(DictationTranscript::Ready("twenty one".to_owned())),
    );
    assert_eq!(
        dictation_transcript(transcript("twelve"), &rules, false),
        Ok(DictationTranscript::Ready("twelve".to_owned())),
    );
}

#[test]
fn itn_dot_limitation_is_explicit() {
    assert_eq!(clean("DOT"), Ok(DictationTranscript::Ready("DOT".to_owned())));
    assert_eq!(
        dictation_transcript(transcript("DOT"), &Replacements::default(), true),
        Ok(DictationTranscript::Ready(".".to_owned())),
    );
    let raw_rule = Replacements::from_pairs([("DOT".to_owned(), "Graphviz".to_owned())]);
    assert_eq!(
        dictation_transcript(transcript("DOT"), &raw_rule, true),
        Ok(DictationTranscript::Ready(".".to_owned())),
    );
}
```

```bash
cargo test --locked --offline dictation_transcript::tests::replacements_preserve_identity_and_sentence_wrappers
cargo test --locked --offline dictation_transcript::tests::normalized_patterns_match_but_values_are_not_normalized_again
```

Expected: wrapper/order assertions fail. The DOT test is a characterization of a known limitation, not a desired transcription result.

- [ ] **Step 2: Preserve original pattern keys and prevent matches across punctuation.**

Add `original_pattern: String` to `ReplacementRule`. In `from_pairs`, build the normalized pattern with `word_core(...).to_ascii_lowercase()` as before, but reject only a zero-token pattern. Store the original string without stripping technical characters. Retain the existing longest-pattern sorting. Serialization uses `rule.original_pattern` as the map key.

The replacement construction inside `filter_map` becomes:

```rust
let tokens: Vec<String> = pattern.split_whitespace()
    .map(|token| word_core(token).to_ascii_lowercase())
    .collect();
if tokens.is_empty() { return None; }
Some(ReplacementRule {
    original_pattern: pattern,
    pattern: tokens,
    replacement,
})
```

Use this `match_at` implementation:

```rust
fn match_at(&self, tokens: &[&str], index: usize) -> Option<(usize, &str)> {
    self.rules.iter().find_map(|rule| {
        let end = index.checked_add(rule.pattern.len())?;
        if end > tokens.len() { return None; }
        let matched = rule.pattern.iter().enumerate().all(|(offset, expected)| {
            let parts = token_parts(tokens[index + offset]);
            let interior_mark = rule.pattern.len() > 1
                && parts.core.chars().any(|c| matches!(c, '.' | '?' | '!' | ';' | ':'))
                && parts.core.chars().all(|c| matches!(c, '.' | '?' | '!' | ';' | ':'));
            !interior_mark
                && (offset == 0 || parts.leading.is_empty())
                && (offset + 1 == rule.pattern.len() || parts.trailing.is_empty())
                && parts.core.eq_ignore_ascii_case(expected)
        });
        matched.then_some((rule.pattern.len(), rule.replacement.as_str()))
    })
}
```

A multiword match cannot swallow interior wrappers or punctuation. A single-token punctuation replacement remains possible.

- [ ] **Step 3: Preserve outer wrappers and avoid a duplicate sentence-mark suffix.**

Add:

```rust
fn wrapped_replacement(first: &str, last: &str, replacement: &str) -> String {
    let leading = token_parts(first).leading;
    let trailing = token_parts(last).trailing;
    let marks_end = trailing.char_indices()
        .find(|(_, c)| !matches!(c, '.' | ',' | '?' | '!' | ';' | ':'))
        .map(|(index, _)| index)
        .unwrap_or(trailing.len());
    let marks = &trailing[..marks_end];
    let trailing = if !marks.is_empty() && replacement.ends_with(marks) {
        &trailing[marks_end..]
    } else {
        trailing
    };
    format!("{leading}{replacement}{trailing}")
}
```

Replace only the matched branch in `apply_replacements`:

```rust
if let Some((length, replacement)) = replacements.match_at(tokens, index) {
    if !replacement.is_empty() {
        output.push(wrapped_replacement(
            tokens[index], tokens[index + length - 1], replacement,
        ));
    }
    index += length;
} else {
    output.push(tokens[index].to_owned());
    index += 1;
}
```

Empty values intentionally remove attached wrappers. Do not add implicit deletion of neighboring standalone punctuation.

- [ ] **Step 4: Move ITN before replacements and document the intentional test change.**

After producing `without_fillers`, use:

```rust
let content = without_fillers.join(" ");
let normalized = if itn {
    normalize_sentence_with_options(
        &content,
        NormalizeOptions::new().with_disable_bare_second(true),
    )
} else {
    content
};
let normalized_tokens: Vec<&str> = normalized.split_whitespace().collect();
let cleaned = apply_replacements(&normalized_tokens, replacements).join(" ");
```

Retain existing `NoSpeech`, raw-empty, and final-empty handling. Update the module order comment.

Rename `inverse_text_normalization_runs_after_replacements` to `replacement_values_bypass_normalization`; its existing `a dozen -> twenty one` input now expects `I have twenty one apples`. This is a deliberate compatibility change, not a removed test.

- [ ] **Step 5: Test serialization and exact application delivery.**

Add in `dictation_transcript::tests`:

```rust
#[test]
fn technical_pattern_keys_survive_serialization() {
    let rules = Replacements::from_pairs([
        ("C".to_owned(), "cee".to_owned()),
        ("C++".to_owned(), "cpp".to_owned()),
        ("C#".to_owned(), "csharp".to_owned()),
        (".env".to_owned(), "environment".to_owned()),
    ]);
    let value = serde_json::to_value(&rules).unwrap();
    assert_eq!(value["C++"], "cpp");
    assert_eq!(value["C#"], "csharp");
    assert_eq!(value[".env"], "environment");
    let restored: Replacements = serde_json::from_value(value).unwrap();
    assert_eq!(rules, restored);
}
```

Add in `config::tests`, which has access to `Config` and existing imports:

```rust
#[test]
fn replacement_table_keeps_technical_keys_in_toml() {
    let input = "\"C++\" = \"cpp\"\n\"C#\" = \"csharp\"\n\".env\" = \"environment\"\n";
    let rules: crate::dictation_transcript::Replacements = toml::from_str(input).unwrap();
    let encoded = toml::to_string(&rules).unwrap();
    let restored: crate::dictation_transcript::Replacements = toml::from_str(&encoded).unwrap();
    assert_eq!(rules, restored);
}
```

Use Task 2's `observed_daemon` and spies for exact cleaned output. Tests live inside `app.rs`, so they may set private test state before starting:

```rust
#[test]
fn protected_technical_text_is_delivered_once_and_stored_exactly() {
    let (mut daemon, effects) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(StaticRecognizer::new("um ER diagram in C++.")));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(effects.lock().unwrap().sent.is_empty());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let effects = effects.lock().unwrap();
    assert_eq!(effects.sent, vec!["ER diagram in C++."]);
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.records[0].transcript, "ER diagram in C++.");
}
```

Run:

```bash
cargo test --locked --offline dictation_transcript::tests
cargo test --locked --offline config::tests
cargo test --locked --offline app::tests
cargo test --locked --offline
git diff --check
git add src/dictation_transcript.rs src/config.rs src/app.rs
git commit -m "fix: preserve replacement punctuation and configured values"
```

## Task 6: Document compatibility and verify the complete repair

**Files:** Modify `README.md`; create `docs/adr/0013-dictation-integrity.md`. No additional production changes are planned in this task.

**Interfaces:** Describe the existing CLI/config schemas and the intentional content/ownership changes. Do not claim deployment or model performance.

- [ ] **Step 1: Add concise user guidance and the follow-up ADR.**

Add the following prose near README's audio/text configuration section, replacing any contradictory ordering description rather than duplicating it:

```markdown
Capture uses a bounded queue. If the queue loses audio or the input backend
reports a stream error, nvstt rejects that session. It does not type or save
a partial transcript. Start another session after correcting the input issue.

The five-second queue capacity is provisional. It does not guarantee a
five-second recording limit or any particular insertion latency.

Cleanup removes clear lowercase or title-case `uh` and `um` variants.
Ambiguous words, uppercase acronyms, and technical symbols remain.

Inverse text normalization runs before `[text.replacements]`. Patterns see
normalized text when `text.itn = true`. Replacement values receive no later
normalization. Existing replacement patterns may need adjustment.

ITN can still change literal technical words. For example, `DOT` becomes `.`.
Set `text.itn = false` when literal technical wording matters more than
spoken-number and punctuation conversion. This repair does not change your
configuration automatically.
```

Write the new ADR with:

```markdown
# Preserve dictation integrity

Status: accepted with the integrity repair implementation

This decision supersedes ADR 0011's broad filler-removal guarantee and
ADR 0012's stutter eligibility, token matching, and normalization order.
Other parts of those decisions remain unchanged.

Use one owned consumer and a bounded SPSC queue for microphone audio.
Count known queue loss. Treat reported backend errors and duration-limit
violations as failed capture. Stop the producer before final integrity checks.
Do not store or deliver a partial transcript from failed capture.

Pre-roll contains only samples not already emitted. Keep detector settings
and one ASR stream per dictation unchanged.

Preserve ambiguous tokens and technical characters. Limit short-stutter
collapse to qualifying alphabetic tokens, excluding uppercase multi-letter
acronyms. Apply replacements after ITN and preserve sentence wrappers.
Keep original replacement keys when serializing configuration.

## Consequences

- The public CLI, IPC, and history schemas stay unchanged.
- The Rust audio source is no longer cloneable; acquisition is single-use.
- Existing replacement files remain readable, but normalized matching can
  require different patterns. Do not rewrite those files automatically.
- `DOT` remains a known ITN limitation. This decision does not promise
  general technical-language preservation under ITN.
- The five-second queue capacity requires paced-input measurement.
- Unit tests do not prove WER, native-detector behavior, target-window
  correctness, or the stop-to-insertion latency requirement.
```

- [ ] **Step 2: Run CPU verification and check only changed-file formatting.**

```bash
cargo test --locked --offline
cargo check --locked --offline --all-targets
rustfmt --edition 2024 --check src/recorder.rs src/app.rs src/speech_gate.rs src/dictation_transcript.rs src/config.rs
git diff --check
```

If rustfmt reports pre-existing formatting outside changed hunks, record the baseline limitation. Do not reformat unrelated files or large adjacent blocks merely to make a repository-wide formatting command green. Correct formatting introduced by these tasks.

- [ ] **Step 3: Check CUDA compilation only if the native runtime already exists.**

Do not run the CUDA installer. Use the same library location as `scripts/build_cuda_companion.sh`:

```bash
runtime_dir="${XDG_DATA_HOME:-$HOME/.local/share}/nvstt/cuda-runtime-12.6.3-cudnn-9.3.0.75"
if [ -f "$runtime_dir/sherpa/libsherpa-onnx-c-api.so" ]; then
    SHERPA_ONNX_LIB_DIR="$runtime_dir/sherpa" \
      cargo check --locked --offline --all-targets --no-default-features \
      --features cuda-runtime --target-dir target/nvstt-cuda-check
else
    printf '%s\n' 'CUDA compile check blocked: preinstalled sherpa runtime is absent.'
fi
```

Record a blocked check as blocked, not passing. A successful compile does not establish GPU inference or available runtime memory. Do not use `--all-features`; this repository intentionally rejects simultaneous CPU/CUDA features.

- [ ] **Step 4: Review the diff against the spec, then commit documentation.**

```bash
git diff --stat 7daf0bb..HEAD
git diff --check
git status --short
git add README.md docs/adr/0013-dictation-integrity.md
git commit -m "docs: explain capture integrity and text compatibility changes"
```

Do not use `git add .`. Review each code commit as well as the current diff. Ensure no model/config/service changes occurred.

## Completion report and deferred validation

Report exact test commands, counts, new regression coverage, blocked checks, intentional compatibility changes, and commit IDs. Do not install the binary.

The remaining product checks are separate work:

- Measure queue occupancy, drain gaps, failures, and actual stop-to-insertion timing with paced audio.
- Compare identical gate-on/off recordings around 25 and 30 seconds, with short final utterances and partial-frame stops.
- Keep failed sessions in accuracy and latency reports.
- Repair notification waiting before capture stop, permission preflight, and wrong-window risks before declaring the KDE workflow acceptable.
- Revisit ITN's acronym behavior, evaluator parity, optional denoise tail, and model-install readiness in their own tasks.

The product target remains below one second ideally, up to two seconds for a measured accuracy gain, and no accepted runs above 2.5 seconds. This implementation plan does not certify that target.

## Plan self-review coverage

| Spec requirement | Tasks |
|---|---|
| Owned consumer, fixed live drain, fresh sessions | 1 |
| Known queue loss, backend errors, duration accounting | 1, 2 |
| Producer shutdown, final failure precedence, no effects on failure | 1, 2 |
| Successful next-session recovery and final-only call counts | 2, 5 |
| Unemitted pre-roll, exact bridge, framing/reset preservation | 3 |
| Conservative fillers/stutters, technical symbols | 4 |
| Replacement identity, wrappers, empty values, serialization | 5 |
| ITN order, mandatory examples, explicit DOT limitation | 4, 5, 6 |
| Compatibility and ADR follow-up | 6 |
| CPU/CUDA build checks without installation | 6 |
| Native timing and detector claims deferred | 3, 6, completion report |
