# Preserve complete audio and measure recognition

Status: accepted, 2026-10-04

This decision supersedes ADR 0010's default model and speech-filtering policy.
It supersedes ADR 0013's frame-routing and broad text-normalization policy.
Capture integrity, bounded history, and recent-audio retention remain separate responsibilities.

## Evidence

Replay of the seven retained attempts reproduced their saved transcripts, including one empty result.
Disabling frame filtering recovered omitted speech in the longest clip.
Adding final silence recovered a missing ending with the Nemotron decoder.
All seven capture reports had zero recorded queue loss and no backend error.
Those reports do not prove that the microphone captured every spoken word.

The native final-word regression failed before the repair and passed afterward.
The speech-detector test verifies exact waveform preservation across partial frames and repeated sessions.
The finalization recovery test catches a recognizer left active after an error.

## Decisions

- Use Parakeet Unified 1120 ms with four threads as the default. Keep other supported models available for measured comparisons.
- Pass the complete waveform to recognition. Optional VAD classifies the session without deleting quiet regions or pauses.
- Add 1280 ms of ASR-only padding before Nemotron EOF. Its decoder requires a complete feature window.
- Add 2240 ms of ASR-only initial silence for Parakeet Unified. This repairs the reproduced short-utterance omission.
- Let Parakeet Unified handle its own final feature window. Keep padding out of captured audio and storage.
- Treat a missing native result as an error. A valid empty result means no recognized speech.
- Flush RNNoise delay at EOF and preserve partial-frame duration.
- Cancel recognizer state after every failed finalization, including errors while feeding the final audio.
- Format numeric spans conservatively. Do not infer times from ranges or rewrite technical words as punctuation.
- Reject invalid configuration types. Support both flat serialized fields and the documented section form.
- After an ambiguous native delivery failure, copy the complete transcript without another automatic typing attempt.
- Retry an established but failed native connection on a later dictation. Bound clipboard subprocess time.
- Write history privately and atomically, with file and directory synchronization.
- Keep private replay data outside Git. Require reviewed references before reporting WER.

## Validation

See [reliability checks](../reliability.md) for the corpus format, runner, thresholds, and native tests.
The seven-clip differential corpus catches known omissions. It is not a representative accuracy benchmark.
Production and competitor claims require reviewed holdout speech and live desktop insertion tests.
