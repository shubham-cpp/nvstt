# Dictation integrity repair

Status: revised after independent review; awaiting user approval.

This revision addresses the review's five implementation blockers. Queue sizing, native-detector behavior, and end-to-end latency remain measurement requirements. ITN preservation is explicitly limited below, not assumed.

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

The audio data callback must not wait for the consumer, allocate memory, or run inference. It converts each interleaved input frame to one mono sample and enqueues it.

The initial queue capacity is five seconds of mono input at the device sample rate. This value is provisional, not a measured safe capacity or latency bound. Samples already drained into processing buffers sit outside this bound.

Keep the existing 30-minute session limit separate. Count all input frames toward that limit, including frames that could not be enqueued. Express queue capacity and loss in mono samples, not interleaved channel samples.

Use a maintained queue implementation rather than adding custom unsafe synchronization. The implementation plan must name its crate and version and verify ownership, full-queue behavior, and callback operations against its source or documentation before adding it.

Keep the queue private to `src/recorder.rs`. Give the worker exclusive ownership of the consumer endpoint. Remove `AudioSource` cloneability and permit one consumer acquisition per session. A second acquisition must return an existing-shape state error. Small Rust recorder-interface changes are allowed; CLI and IPC changes are not.

Each live drain consumes at most the number of samples available at entry. It must return while new audio arrives, so the worker can check stop or cancel commands. Allocation while draining is allowed on the worker, not in the audio data callback.

Create fresh queue and failure state for every session. Old source handles must not observe or consume a new session.

### Failure behavior

Retain three distinct failure conditions until the session ends:

- Queue-full loss, with an atomic mono-sample count.
- A backend capture error, with an unknown loss count unless the backend supplies one.
- The existing duration-limit failure.

Treat any reported backend stream error as an interrupted capture in this phase. Do not merely log it and deliver the remaining audio. Do not invent a sample-loss count for that error.

The backend error callback must publish a sticky atomic failure flag before optional diagnostic handling. Stop must fail even if the existing diagnostic message slot could not be updated. The strict allocation-free data-path promise applies to the audio data callback, not CPAL internals or the backend error callback's existing diagnostic storage. Do not claim a fully real-time-safe backend error path.

A session with a capture-integrity failure must produce failed transcription, delivery-not-attempted, no transcript, and no history append. Retain the existing IPC response shape. Include known loss counts and failure categories without transcript content.

Capture-integrity checks must run even when the worker already has a recognition error. If both exist, report capture-integrity failure as the primary error. Report all known capture conditions together rather than hiding a backend error behind a queue count.

Stop and synchronize producer shutdown before reading final failure state. Then:

1. Read the stable capture-integrity result, independent of healthy-drain helpers.
2. If capture failed, cancel/reset recognition without further decoding and return failure.
3. Otherwise drain the now-finite remaining queue and finalize recognition.
4. Return the daemon to idle on either path.

Atomic updates must become visible before the stable post-shutdown read. Document the selected queue and CPAL backend guarantees in the implementation plan. Preserve CPAL's existing Linux shutdown synchronization rather than assuming stream drop is currently unsafe.

Cancellation also stops the producer and releases the consumer safely when the queue is full or the worker has failed. Cancellation produces no transcript or history. The next session gets new queue, counters, and failure state.

The fail-whole-session policy intentionally provides no recoverable partial transcript in history. Do not add automatic partial copying or audio retention as a workaround.

### Checks

Use barriers and small queues instead of sleep-based concurrency assertions.

- Enqueue and drain numbered samples concurrently; assert exact order and count.
- Keep producing during a live drain; assert that the drain remains bounded.
- Fill a small queue; assert a stable loss count after shutdown.
- Inject a backend error after valid audio with no queue overflow; assert session failure.
- Combine capture and recognizer failures; assert capture-error precedence.
- Finish while a callback is in progress, including its final sample and failure event.
- Reject a second consumer acquisition and isolate old handles from a new session.
- Verify stereo downmix, final queued samples, full-queue cancellation, and repeated sessions.
- Test duration boundaries independently of queue size and enqueue success.

At the app boundary, use a counting text sink and a history spy. Assert zero sink calls before stop, exactly one on successful stop, and zero sink/history calls on capture failure or cancellation. Assert the failure status, idle recovery, and successful delivery in the next session.

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

These tests establish duplicate-free `GateState` routing under supplied detection decisions. They do not establish complete native-detector preservation.

Retain framing, partial-final-frame, flush, cancellation, and reset tests for `SpeechGate` and `VadGatedRecognizer`. A later native-detector experiment must check stop-time flushing, short final utterances, and behavior near 30 seconds of actual speech. The reported 25-second errors remain unexplained.

## 3. Conservative text cleanup

### Behavior

Prefer retaining an occasional hesitation over removing valid content. The guarantees in this section concern nvstt's token cleanup and explicit replacement values. They do not imply that the existing ITN dependency preserves all technical language.

Limit automatic filler removal to lowercase or title-case forms of `uh`, `uhh`, `uhhh`, `um`, `umm`, and `ummm`. Do not classify all-uppercase tokens or ambiguous tokens such as `mm`, `ER`, `hm`, and `hmm` as fillers.

Keep punctuation-only tokens instead of discarding them through an empty-word filter. Punctuation-only output remains deliverable unless an explicit replacement deletes it.

Keep intentional doubles such as `very very`. The existing three-or-more short-stutter rule applies only when every token is an eligible one- or two-letter alphabetic token. Exclude uppercase acronyms of two or more letters and tokens containing technical punctuation. Preserve `ER ER ER` and mixed `C`/`C++` runs. Keep the existing `I I I I think` repair.

### Token identity and replacement punctuation

Use one token-identity rule for replacement pattern compilation, input matching, and serialization. Keep original pattern spelling for serialization; do not serialize a stripped form that loses technical characters. Matching remains case-insensitive and longest-match-first.

Treat edge quotes, enclosing brackets, and trailing sentence marks as detachable wrappers. Do not strip `+`, `#`, `_`, `/`, or a leading dot from a token. Preserve internal dots and apostrophes. A trailing period is a sentence mark; literal filenames ending in a period are outside this limited matching policy. This is not a programming-language tokenizer.

Phrase replacements must not cross an interior sentence boundary marked by `.`, `?`, `!`, `;`, or `:`. They must not swallow punctuation between matched words. Preserve outer wrappers around a nonempty replacement.

If a nonempty replacement already ends with the exact trailing sentence mark from the input, do not append a duplicate. Otherwise preserve that mark. An empty replacement removes the matched phrase and its attached wrappers, without leaving orphan punctuation. Standalone punctuation tokens outside the match remain unchanged.

Add explicit cases for quoted phrases, sentence-final matches, empty values, and punctuation-bearing values. Do not infer these rules solely from the old `word_core` helper.

### ITN ordering and limited guarantees

Apply user replacements after inverse text normalization. Replacement values then receive no later normalization. This protects the configured value, not a pattern or technical token that ITN already transformed.

Document that patterns see normalized text when ITN is enabled. Existing files remain readable, but some rules will need different patterns. Do not rewrite user configuration automatically.

The locked `text-processing-rs` 0.2.2 normalizer includes case-insensitive spoken-punctuation conversion. Its source maps `DOT` to `.` when parsed as punctuation. Moving replacements cannot by itself protect the original `DOT` pattern.

Leave general ITN acronym protection outside this phase instead of adding an unreviewed masking or token-segmentation mechanism. Record `DOT` as a known limitation with ITN enabled. Literal technical dictation can use the existing `text.itn = false` option; this phase does not change that setting for the user. With ITN off, literal `DOT` must survive unchanged. With ITN on, record the dependency's actual result in a characterization test and documentation, not as evidence of correct acronym preservation.

Do not add a grammar model, fuzzy correction, vocabulary learning, or paraphrasing. Keep the ITN default unchanged. A later technical-dictation acceptance review must revisit the known ITN limitation.

### Checks

Test the full cleanup function, not only a token filter. Require these preservation cases with ITN both off and on:

- `5 mm` retains its unit and `ER diagram` retains its acronym.
- `a + b = c` retains its symbols.
- `C++`, `C#`, `.env`, and `config.rs` survive unrelated replacements.
- `ER ER ER`, mixed `C`/`C++` repetitions, and `very very` remain intact.
- Clear `um` and `uh` hesitation tokens can still be removed.
- Explicit replacement values that resemble numbers or spoken punctuation remain as configured.

If ITN breaks any of these mandatory cases, stop and revise the design. Do not silently weaken the assertion or claim a general preservation guarantee.

Also test distinct `C`, `C++`, and `C#` rules; phrase boundaries; quoted matches; duplicate trailing marks; empty values; punctuation-only input; and replacement serialization round trips. Test a pattern transformed by ITN and its normalized counterpart. Include the documented `DOT` limitation separately.

Use the app spies described above to prove one final delivery, with the exact cleaned string, and unchanged successful-history behavior.

## Testing and release boundary

All build, test, and shell commands run inside the `Fedora` distrobox.

Write regression tests first. Run them against unchanged implementation and confirm the intended failures. Then implement the smallest repair and rerun the focused tests and full suite.

The review baseline had 76 passing tests. Tests must not require a microphone, model downloads, desktop permissions, or network access at runtime. Fetching a build dependency, if necessary, is separate from audio processing.

Review changed functions for concurrency safety, intentional Rust API changes, and added callback work. Do not claim improved WER or subsecond insertion from unit tests.

The evaluation command scores raw ASR and bypasses live capture. The WAV example directly uses an ungated recognizer. Neither replaces the capture, gate, or app-boundary regression tests above.

### Compatibility and affected modules

- `src/recorder.rs` changes queue ownership, source acquisition, and capture-error handling. `src/app.rs` must adapt worker startup, bounded draining, shutdown, and recovery.
- `Cargo.toml` and `Cargo.lock` gain the selected queue dependency. Review compatibility with both CPU and CUDA build configurations.
- `src/speech_gate.rs` changes the samples received by `VadGatedRecognizer` in `src/recognizer.rs` and gated evaluation. The recognition trait need not change.
- `src/dictation_transcript.rs` changes final transcript content, replacement serialization, and matching. Update configuration and application tests accordingly.
- History and IPC schemas stay unchanged, but transcript contents and capture-failure outcomes change. Text sinks need no new production interface; tests need counting spies.
- Update README guidance and add an ADR that explicitly supersedes the relevant parts of ADRs 0011 and 0012. Explain narrower filler removal, acronym-safe stutters, normalized replacement patterns, and whole-session capture failure.

Replace the existing test that requires ITN after replacements with tests for the intentional new ordering. Do not delete it without documenting the compatibility change. Do not assume external users of the public Rust recorder types remain compatible.

Do not install the repaired binary automatically. Present the test results and diff before any change to the user's active daemon.

## Deferred work

The next phase will address evaluator parity and measured latency. It includes the noise-category classification defect, raw versus cleaned scoring, paced audio, and stop-to-completed-insertion timing.

Desktop notification waits, portal preflight, wrong-window prevention, optional denoiser tail flushing, and stronger model installation checks remain separate repairs.

Notification waiting before recorder stop can extend capture beyond the user's stop request. Portal setup can exceed ten seconds. These remain product-acceptance blockers even if integrity tests pass. Final-only delivery does not prove delivery to the app focused at stop.

Measure the provisional queue capacity with paced input, consumer stalls, queue occupancy, drain gaps, loss, and actual stop-to-insertion time. Test below and above capacity. Include failed sessions in the results rather than reporting only successful latency and accuracy runs.

After those checks, compare speech gating on/off, CPU/CUDA, streaming profiles, and selected alternative models. Keep identical recordings for each comparison.

No recording tools were found through `command -v` for `ffmpeg`, `ffprobe`, or `pw-record` inside Fedora during preparation. No test audio was found under the inspected `dist` or `tests` paths. Existing `examples/transcribe_wav.rs` remains available.

Personal corpus recording therefore needs a separate agreed setup step. It is not a prerequisite for the deterministic integrity repairs above.

## Acceptance

This phase is accepted when regression tests demonstrate:

- Ordered application capture within queue capacity, plus failure for known queue loss, reported backend capture errors, and duration-limit violations.
- Exclusive consumer ownership, bounded live drains, synchronized shutdown, and isolated repeated sessions.
- No delivery or history on capture failure, and successful next-session recovery.
- Duplicate-free `GateState` routing under supplied detection decisions.
- The mandatory text-preservation and replacement cases above, with the ITN limitation recorded explicitly.

These checks cannot detect unreported hardware loss or establish complete native-detector behavior. They do not certify general technical-language preservation under ITN.

The existing final-only workflow must remain intact. The model, installed daemon, and user configuration must remain unchanged.

The broader product target remains below one second ideally, up to two seconds for a measured accuracy gain, and no accepted runs above 2.5 seconds. This phase does not certify that target.
