# Recent dictation audio for diagnosis

Status: proposed for user review.

## Purpose and scope

Keep the last seven stopped dictation recordings so a future word-loss report has audio evidence. This is a diagnostic feature, not a fix for lost words. Keep the current model, speech-gate policy, transcription flow, final-only delivery, ten-record text history, and public IPC format. Do not save canceled sessions or sessions that never started capture.

The user approved automatic storage in `$XDG_STATE_HOME/nvstt/recordings/`, with `~/.local/state/nvstt/recordings/` as the existing XDG fallback. The user accepts the disk and temporary memory cost of an uncommon 30-minute session. Usual sessions last 90–120 seconds.

## Existing path and evidence

`src/recorder.rs` converts microphone callbacks to mono `f32` and pushes them into a five-second ring. `src/app.rs` drains the ring in a recognition worker, then stops capture, drains remaining samples, finishes the audio pipeline, and finalizes recognition. On a capture error, it currently cancels the worker without saving its audio. `src/history.rs` persists up to ten text records, but `HistoryRecord` in `src/domain.rs` has no audio field. `src/audio.rs` can read 8-bit and 16-bit PCM WAV files for evaluation but cannot yet read float WAV.

The source review and first-party comparisons are in `docs/research/12-handy-voxtype-boundary-comparison.md`, `docs/research/13-sherpa-vad-stream-finalization.md`, and `docs/research/14-capture-reliability-comparison.md`. They do not prove a cause for the user's missing words. Audio can help separate capture, gating, ASR, and text cleanup, but it cannot contain speech from before the first microphone callback or after stop.

## Options

1. **Recommended: copy drained input audio into worker-owned memory; save after stop.** This adds no disk work to the microphone callback or live recognition path. At 48 kHz, a two-minute mono `f32` session uses about 23 MiB of temporary memory; 30 minutes uses about 330 MiB. The existing 30-minute capture limit bounds normal collection. Saving after stop can add finalization time, which must be measured.
2. Write audio during recognition. This uses less RAM, but a slow disk could stall the worker and fill the existing capture ring. A separate writer and another bounded queue would add failure cases. Do not use this path without evidence that post-stop saving is too slow or memory is unacceptable.
3. Save only speech-gated audio. This is smaller but cannot show audio the gate removed. It does not meet the diagnostic goal.

## Capture and stop flow

The worker keeps the original mono samples it drains from `AudioSource`, before resampling, denoise, VAD, or ASR. It copies them after each bounded queue drain; the CPAL callback remains unchanged. Store the source sample rate with the samples. Do not save a resampled or gated substitute as the original recording.

If recognition or audio processing fails while capture continues, the worker must still drain and retain later audio until stop. Do not feed a failed recognizer again. Preserve current error precedence: known capture-integrity failure takes priority over a recognizer failure. Queue overflow still fails the dictation instead of delivering a partial transcript. The recording contains only samples successfully enqueued and drained. Mark it partial when capture reports loss, a backend error, a duration-limit breach, or an incomplete drain; do not claim it is the full microphone input.

When the user stops, stop and synchronize the recorder producer, then drain the remaining source even if recorder stop reports an error. A stop error must still prevent transcription success and delivery. The worker must return its collected audio and any capture/recognition failure separately so the app can save audio without turning a failed dictation into a success. Successful transcript, no-speech, empty-transcript failure, recognizer failure, and capture failure each count as one stopped attempt. A canceled session discards its samples and creates no recording. If the worker panics before it returns audio, report that no recording was saved instead of publishing a misleading empty WAV.

Save after the worker returns and before the stop command completes. Successful final-only delivery and existing history rules remain independent of storage. If audio saving fails, continue transcription and delivery normally and append a clear storage warning to the existing command result message. On a transcription failure, keep the original failure as the primary result and add the storage warning. Do not report a recording path as saved unless its entry is complete. A process crash before commit may lose that session's audio; the feature does not promise crash-proof capture.

## File layout, privacy, and retention

Use one directory per session under `recordings/`, named with a sortable stop time and the existing session ID. Each committed directory contains `audio.wav` and `metadata.json`. The WAV holds mono IEEE float32 samples at the input device's sample rate; this preserves the mono samples given to the worker without another quantization step. A zero-sample stopped session remains a valid, marked recording. Detect WAV size limits and report a save failure rather than writing an invalid file.

Metadata records a format version, session ID, stop time, sample rate, frame count, selected model and profile, gate/denoise/ITN settings, capture completeness, and transcription outcome. It records a known dropped-sample count and capture failure category when available. It does not duplicate the transcript, audio, or clipboard contents. The session ID ties successful attempts to existing text history; failed and no-speech attempts remain discoverable in `recordings/` without adding text history entries. Users and future diagnostics can list the directory and supply a WAV path; no new CLI command or IPC field is required.

A recording-store module owns WAV encoding, metadata, permissions, staging, and retention. The daemon calls it once per stopped session. Do not add a storage trait for a single implementation.

Create the state and staging directories with mode `0700` and the WAV and metadata with mode `0600`. Do not log audio or transcript contents. Audio is not encrypted; normal account access and external backups may still copy it. Write both files to a private staging directory, close and sync them, and publish the complete directory with an atomic rename on the same filesystem. Do not delete any prior recording until the new entry is complete. Then keep the seven newest committed entries by stop time, using the session ID to break ties. On startup, remove this feature's abandoned staging entries and reconcile any committed entries beyond seven after an interrupted prune. Never delete unrelated files in the state directory.

## Replay and checks

Extend the existing WAV reader in `src/audio.rs` to accept mono float32 IEEE WAV alongside its current PCM formats. The private `nvstt model evaluate` command can then read a saved WAV without a conversion tool. It replays captured audio through the selected evaluation configuration; metadata helps select the original model settings. Replay is diagnostic: evaluation does not recreate callback timing, queue loss, audio not captured by CPAL, or old model/configuration files that have since changed. Do not automatically upload a recording or add it to text history.

Write tests before implementation for:

- Exact first and last successfully drained samples, input sample rate, float32 WAV round-trip, zero-length audio, and delayed samples in the queue at stop.
- Recording of success, no-speech, empty transcript, recognizer failure, recorder stop error, backend error, and known queue loss; cancellation and failed start create no recording.
- Continuing to drain after a recognizer error, stable capture-error precedence, and no transcript/history/delivery after a capture failure.
- Seven-entry rotation, restart reconciliation, incomplete staging recovery, file modes, atomic visibility, and save failure without losing existing recordings or changing transcription/delivery status.
- Replaying a saved float WAV through the current evaluation reader, while old PCM WAV tests still pass.

Run focused tests and the full suite through `mise exec -- cargo ...`. Measure the stop-to-result delay before and after storage on 90–120 second sessions, including a slow-disk or injected save-failure case. If post-stop saving causes an unacceptable delay on the user's machine, revisit the save scheduling before shipping rather than moving disk writes into the live recognition worker. Unit tests and WAV replay alone cannot prove that boundary words are preserved.

## Not in this change

Do not add background microphone capture, capture after stop, new model or VAD defaults, encryption or key management, permanent audio archives, network uploads, or automatic transcript correction. Diagnosis of the current first/last-word loss remains a separate task that needs a reproducing recording and timing evidence.
