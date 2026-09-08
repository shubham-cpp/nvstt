# Dictation integrity repair

Status: proposed for user review.

## Goal

Repair confirmed audio and text integrity defects before comparing speech models. Keep the current CLI and Nemotron model selection.

This phase must preserve the final-only delivery contract. It must not record personal audio, install models, change user configuration, or restart the installed daemon.

## Why this phase comes first

The implementation review found three independent corruption paths:

- The microphone callback silently returns when the capture mutex is busy.
- Speech-gate restart can replay audio that the recognizer already received.
- Text cleanup deletes valid units, acronyms, and symbols.

These defects can distort model comparisons. They do not yet explain the reported increase in errors after about 25 seconds.

Alternatives considered:

1. **Repair integrity first, recommended.** Deterministic regression tests can establish these improvements without personal recordings.
2. **Switch models first.** This could improve recognition, but damaged input or output would obscure the result.
3. **Add an LLM cleanup pass.** This adds latency and can change meaning. It does not repair lost audio.

## Existing flow

The microphone callback downmixes audio. A worker drains capture, converts the sample rate, applies optional denoise, and feeds the speech gate and recognizer.

Stop finalizes recognition. Cleanup produces one transcript, which the daemon stores and delivers through the existing text sink.

Preserve those module boundaries. Do not change the public CLI, IPC schema, recognition trait, model registry, or history format.

## 1. Capture handoff

### Behavior

Replace the callback/consumer shared sample mutex with a preallocated, bounded, single-producer/single-consumer audio queue.

The callback must not wait for the consumer, allocate memory, or run inference. It converts each interleaved input frame to one mono sample and enqueues it.

The queue holds five seconds of mono input at the device sample rate. This is a backlog safety bound, not a latency promise. Keep the existing 30-minute session limit separate.

Use a maintained queue implementation rather than adding custom unsafe synchronization. Keep the queue private to `src/recorder.rs`. Retain the existing recorder and audio-source interfaces where possible.

The recognition worker remains the sole consumer. Allocation while draining is allowed on that worker, outside the callback.

### Failure behavior

Count queue-full sample loss with an atomic counter. A session with any dropped audio must fail transcription instead of delivering a plausible but incomplete transcript.

Use the existing error response shape. Include the dropped-sample count in the error message without including transcript content.

The failure flag remains set until the session ends. Finalization must check it after capture stops and before delivering text. A new session resets the queue and counters.

Keep cancellation and recorder shutdown safe when the queue is full or the consumer has stopped.

### Checks

- Enqueue and drain numbered samples concurrently; assert exact order and count.
- Fill a small test queue; assert explicit loss reporting and no final delivery.
- Verify stereo downmix, final queued samples, cancellation, and repeated sessions.
- Retain the existing capture duration limit.

## 2. Speech-gate handoff

### Behavior

Pre-roll must contain only audio not already emitted to the recognizer.

When an active gate emits a frame, do not retain that frame for a future restart. When an inactive gate opens, emit its buffered, never-emitted pre-roll once and clear it.

Keep current detector thresholds, 400 ms pre-roll capacity, and 200 ms silence bridge. Do not change the 30-second detector setting during this repair.

Use the existing `GateState` rather than introducing an additional segmentation layer. Do not reset the ASR stream at speech pauses.

### Checks

Use numbered, nonzero sample values to distinguish input audio from the synthetic silence bridge.

- First speech preserves available pre-roll.
- Active speech emits each input sample once.
- A one-frame inactive interval followed by restart does not replay the previous region.
- Longer pauses keep pre-roll bounded and preserve only the intended recent input.
- Repeated transitions do not accumulate duplicate samples.
- Cancellation resets all routing state.

These tests establish routing correctness. A later native-detector experiment must establish what occurs near 30 seconds of actual speech.

## 3. Conservative text cleanup

### Behavior

Preserve technical content even when that means retaining an occasional hesitation.

Limit automatic filler removal to ordinary lowercase or title-case `uh` and `um` variants. Preserve all-uppercase acronyms and ambiguous tokens such as `mm`, `ER`, `hm`, and `hmm`.

Keep punctuation-only tokens instead of discarding them through an empty-word filter. Do not treat technical suffixes such as `++` or `#` as disposable punctuation for stutter or replacement matching.

Keep intentional doubles such as `very very`. Retain the existing narrow short-stutter policy only for plain alphabetic tokens; it must not collapse repetitions of technical tokens such as `C++`.

Preserve sentence punctuation around replacement matches. Match phrase replacements case-insensitively and retain the existing longest-match rule.

Apply user replacements after inverse text normalization so explicitly configured written forms remain unchanged afterward. This is an intentional ordering change. Document that replacement patterns see normalized text when ITN is enabled.

Do not add a grammar model, fuzzy correction, vocabulary learning, or paraphrasing. Do not change the ITN default in this phase.

### Checks

Test with ITN both off and on where applicable:

- `5 mm` retains its unit.
- `ER diagram` retains its acronym.
- `a + b = c` retains its symbols.
- `C++`, `C#`, and filenames survive unrelated replacements.
- A replacement preserves adjacent sentence punctuation.
- An explicit replacement value bypasses later normalization.
- `very very` remains unchanged.
- Clear `um` and `uh` hesitation tokens can still be removed.
- Uppercase acronyms and punctuation-only input are not silently discarded.
- Existing app tests still deliver only one final transcript and retain history behavior.

## Testing and release boundary

All build, test, and shell commands run inside the `Fedora` distrobox.

Write regression tests first. Run them against unchanged implementation and confirm the intended failures. Then implement the smallest repair and rerun the focused tests and full suite.

The review baseline had 76 passing tests. Tests must not require a microphone, model downloads, desktop permissions, or network access at runtime. Fetching a build dependency, if necessary, is separate from audio processing.

Review changed functions for concurrency safety, accidental API changes, and added callback work. Do not claim improved WER or subsecond insertion from unit tests.

Do not install the repaired binary automatically. Present the test results and diff before any change to the user's active daemon.

## Deferred work

The next phase will address evaluator parity and measured latency. It includes the noise-category classification defect, raw versus cleaned scoring, paced audio, and stop-to-completed-insertion timing.

Desktop notification waits, portal preflight, wrong-window prevention, optional denoiser tail flushing, and stronger model installation checks remain separate repairs.

After those checks, compare speech gating on/off, CPU/CUDA, streaming profiles, and selected alternative models. Keep identical recordings for each comparison.

No recording tools were found through `command -v` for `ffmpeg`, `ffprobe`, or `pw-record` inside Fedora during preparation. No test audio was found under the inspected `dist` or `tests` paths. Existing `examples/transcribe_wav.rs` remains available.

Personal corpus recording therefore needs a separate agreed setup step. It is not a prerequisite for the deterministic integrity repairs above.

## Acceptance

This phase is accepted when regression tests demonstrate continuous capture within queue capacity, explicit failure on audio loss, duplicate-free gate routing, and preserved technical text.

The existing final-only workflow must remain intact. The model, installed daemon, and user configuration must remain unchanged.

The broader product target remains below one second ideally, up to two seconds for a measured accuracy gain, and no accepted runs above 2.5 seconds. This phase does not certify that target.
