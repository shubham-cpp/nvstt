# Dictation reliability and archive ownership

Status: proposed for user review.

## Purpose

Make stopped dictation outcomes easier to reason about and test. Correct recording-store failure handling and improve local diagnosis of speech-gate errors. Keep normal dictation responsive without adding disk work during capture.

This design follows the approved recent-audio feature. It does not claim to fix missing words. The gate-on/off replay of one private clip is evidence for a gate effect, not a measured accuracy improvement across users.

## Decisions already made

- Keep original mono audio in worker-owned memory until stop. Do not write audio during live capture or in the microphone callback.
- Save a stopped attempt before delivering its transcript or returning the stop response. Preserve the existing immediate storage warning. Synthetic release testing previously measured about 51 ms added p95 on this host; live insertion delay remains unmeasured.
- Keep seven private WAV entries for stopped attempts and ten separate text-history records. Keep canceled and never-started sessions out of audio storage.
- Keep the Nemotron 560 ms model and the existing speech-gate default until paired, corrected evaluations justify a policy change.
- Keep the history store's existing atomic replacement. Do not add file or directory sync for power-loss durability. The user does not require that guarantee.
- Preserve final-only delivery, current CLI and IPC shapes, privacy modes, and the capture-failure rule from ADR 0013.

## Stopped dictation outcome

Make the stop path distinguish four facts: producer shutdown, capture integrity, recognition result, and raw audio. Read the stop timestamp immediately after the recorder stops or a stop attempt fails. Do not use the later worker-finalization timestamp as the recording stop time.

Change the internal recorder stop result so capture-integrity faults do not masquerade as producer-stop faults. A dropped sample or backend capture error sets its own capture fields. Set `stop_failed` only for a real stop failure. Stop the producer before reading the final capture report and draining the remaining source. A stop failure or capture-integrity failure still prevents transcript delivery and text history.

The recognition worker continues to drain and keep successfully queued audio after a recognizer error. It returns raw audio and the final capture report separately from the recognition outcome. Move its audio buffer into the stopped outcome; do not clone the full recording. The app preserves capture-error precedence over recognizer errors. A worker panic cannot publish a misleading empty WAV.

Keep the daemon as the coordinator. It assembles one stopped outcome from the recorder and worker, then chooses storage, transcription, history, and delivery actions. Do not add a trait or a second state machine for this single path. Tests should observe results through the daemon's existing command interface.

## Recording-store interface and publication

The daemon supplies stopped-session facts: session identity, actual stop time, original samples and sample rate, capture status, selected processing settings, and transcription status. The recording store owns the on-disk format version, derived frame count, file names, metadata serialization, private permissions, publication, and retention. Do not require the daemon to construct versioned disk metadata or infer a warning category by searching its text.

Keep one private staging directory per save. Sync the WAV, metadata, and staging directory before the atomic rename. After rename, attempt the recording-root directory sync. A successful rename produces a published path even if that sync fails; report its durability uncertainty through a typed warning. Do not claim that the audio was not published.

If the root-directory sync fails, do **not** prune older recordings. Keep them until a later successful save or startup reconciliation. A post-publication prune failure also returns the published path with a typed warning. A pre-publication failure returns an error and leaves prior committed recordings unchanged. Reconciliation removes only recognizable, feature-owned staging entries; it must not remove foreign files or follow symlinks.

The store returns structured warning kinds and their error details. The daemon converts them to user text without parsing store-generated prose. Storage errors and warnings must not change the primary transcription or delivery result. Do not log audio or transcript contents.

## Speech-gate diagnosis

Use the existing local model-evaluation path to replay each authorized WAV twice with the same model and profile. Vary only the speech gate. A private manifest with corrected references can compare substitutions, deletions, insertions, first/last words, silent-clip failures, and finalization time. The manifest and audio remain local and uncommitted. Do not upload them.

Add focused tests for gate transitions and finalization using generated or distributable test audio. First establish whether detector decisions, routed samples, or ASR readiness explain an observed error. Add bounded decision/timing observations only if those tests need them; never log raw samples or transcript text by default. The evaluation path cannot reproduce live callback timing, queue loss, or desktop insertion.

One clip and an uncorrected history transcript cannot establish word error rate. Do not switch models, change the default gate, or add ASR padding based only on that clip. Treat the known Nemotron final-chunk risk as a separate test hypothesis, not an assumed cause of the mid-dictation error.

## Tests and performance checks

Write a failing test before each behavior change. Cover these cases:

1. A root-directory sync failure with seven committed entries leaves the old seven in place. A later successful save or reconciliation restores the seven-entry limit without deleting foreign files.
2. A queue overflow or backend capture error sets the matching capture fields but not `stop_failed`. An actual stop failure sets `stop_failed`. Both block history and delivery while retaining available audio.
3. The stored stop time is recorded before slow worker finalization. Keep session ordering stable when two stops are close together.
4. Structured save warnings preserve the published path and the primary transcription/delivery status. Test each warning type and pre-publication failure.
5. Cancellation, no-speech, recognizer failure, failed start, and normal delivery keep their existing audio and history behavior.
6. Gate-on/off evaluation uses the same audio, model, and profile. Pure gate tests exercise boundary frames, short speech, pause bridges, and end-of-stream behavior. No test commits personal audio.

Run the full suite, `cargo fmt --all -- --check`, and strict Clippy through `mise exec --`. Fix the existing format drift and two `clippy::type_complexity` warnings without changing unrelated runtime behavior. Do not hide new warnings with a blanket allow.

Rerun the ignored release benchmark on the persistent host filesystem: generated 48 kHz, 120-second capture, 20 stop-to-result trials with storage and the existing blocked-store control. Report p50, p95, and the limits: the control still attempts a failing save, and the test excludes live input and text insertion. Keep the established 250 ms p95 save-related gate for this synthetic case; investigate any regression before shipping.

Measure peak memory for a representative 120-second session. Calculate the upper bound at each supported input rate before running a long stress test; 30 minutes of raw mono `f32` is about 330 MiB at 48 kHz and 1.29 GiB at 192 kHz, before vector spare capacity and model memory. Do not allocate a 30-minute high-rate fixture on a memory-limited machine. If actual hardware or safe stress tests show unacceptable use, return for a separate memory-budget decision. Do not silently reduce audio fidelity or session length.

## Decision records and scope

The approved recent-audio design explicitly chose automatic seven-entry retention. `CONTEXT.md` and ADR 0002 still say audio is not retained by default. Update the glossary to distinguish text-history retention from recent dictation audio. Record an ADR that supersedes only ADR 0002's no-default-audio rule. Keep its ten-entry history decision. Do not add an opt-in toggle without a new user decision.

This work does not change the model artifact, speech-gate parameters, public configuration keys, CLI, IPC, text-history schema, encryption policy, or delivery backend. It does not promise crash-proof microphone capture, power-loss durability for history, or a cure for word loss. Keep the linked test binary and active daemon unchanged until the user requests a rebuild or restart.
