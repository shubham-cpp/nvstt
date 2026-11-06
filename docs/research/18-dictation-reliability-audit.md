# Dictation reliability audit

Date: 2026-10-04

## Scope and evidence

This audit replayed the seven retained recordings, oldest first. All used Nemotron 560 ms when captured.
All capture reports recorded zero dropped samples and no backend, stop, drain, or duration error.
These counters cannot prove that capture included every spoken word.

The private corpus is under `~/.local/state/nvstt/private-evaluation/reliability-20261004`.
It contains original audio, capture metadata, hashes, and evaluation reports. Audio stays outside Git.
The original history is diagnostic evidence, not a verified reference transcript.

| Clip | Duration | Reproduced problem | Evidence from replay |
| --- | ---: | --- | --- |
| 01 | 18.13 s | Initial wording and ending differed between models | Complete audio restored the opening; intended wording needs review |
| 02 | 5.61 s | Opening became “Line all” | Disabling frame filtering restored “Outline all” |
| 03 | 2.68 s | Empty transcript | Ungated Nemotron and Parakeet TDT recognized “Hello” |
| 04 | 5.46 s | Missing final phrase | Decoder padding restored “things are working” |
| 05 | 4.10 s | Stopped at “whether” | Complete audio and decoder flush recovered more of the ending |
| 06 | 4.61 s | “Hopefully” collapsed to “fully” | Decoder flush recovered “Hopefully everything is” |
| 07 | 27.65 s | Missing middle sentence and ending; number corruption | Complete audio recovered speech; conservative formatting preserved “5 to 8” |

Several clips contain substantial signal in their last 100 ms. A replay cannot recover uncaptured speech.
The differences above establish specific regressions. They do not establish complete transcript accuracy.

## Confirmed defects and changes

### Audio filtering removed speech

The old gate deleted inactive frames and inserted artificial bridges across pauses.
Quiet speech and short utterances could disappear before recognition.
The optional detector now receives audio without removing samples from the recognizer's input.
The default disables this detector. An opt-in native test verifies exact sample preservation.

### Decoder finalization left speech pending

Nemotron's native readiness check requires a complete feature window, including at EOF.
Calling `input_finished` alone did not flush the last speech frames.
The adapter now supplies 1280 ms of recognition-only silence before EOF.
Raw recordings never contain this synthetic padding.

Parakeet Unified has a separate EOF path which pads its final feature window.
Extra trailing silence did not fix its empty result on clip 03.
Adding 1120 ms of initial waveform silence recovered the word.
The integrated 1120 ms prefix still failed. A 2240 ms recognition prefix recovered the short clip in isolation.
Three complete runs then passed all nine clips with stable text and no silent-input failures.
The default now uses Parakeet Unified 1120 ms, four threads, and this initial context.

### Denoising dropped its delayed final frame

RNNoise delays audio by one frame. The pipeline discarded its initial output but did not flush the delayed final output.
The repaired flush preserves both complete and partial frame durations. A regression test failed before this change.
Denoising was off in the seven recordings, so this defect did not cause their omissions.

### Text formatting changed meaning

The former sentence-wide formatter changed “five to eight” into `07:55`.
It could also reinterpret technical words such as `DOT` before replacements ran.
The new formatter handles numeric spans, preserves ranges, and leaves technical text intact.
Configured replacements run after numeric formatting.

### Failed finalization could poison the next session

An error while feeding final audio could leave the recognizer active.
A regression test reproduced the next start failing with “already active”.
Every failed worker finalization now cancels recognizer state.

### Delivery retries could duplicate text

A native send failure can occur after the target receives some text.
The delivery path now copies the transcript after this ambiguous failure.
It does not automatically insert the same transcript through another backend.
Clipboard subprocesses have a deadline. Later dictations can reconnect a failed native backend.
Native send completion still cannot prove that the target application received every character.

### Configuration and storage needed stronger guarantees

Invalid setting types no longer silently select defaults.
Flat serialized denoise and formatting settings now round-trip correctly.
History and portal restore tokens use private atomic writes and synchronization.
WAV loading rejects invalid sample rates, incomplete sample data, and nonfinite samples.

## Model selection evidence

All timings below use CPU inference on this machine. They exclude model startup and desktop delivery.
Finalization latency simulates audio arrival, decoder backlog, and EOF work.
It is not measured hotkey-to-insertion latency.

- Repaired Nemotron 560 ms passed seven phrase checks plus silence and low-noise checks.
- Parakeet Unified 1120 ms, four threads, completed three runs with stable text.
  Simulated p95 finalization was 800, 833, and 835 ms. Clip 03 remained empty in every run.
- Parakeet Unified 560 ms with eight threads accumulated about 13.2 seconds of simulated p95 delay.
- Parakeet TDT v3 recovered clip 03 but produced worse wording elsewhere in this differential comparison.
  Its measured p95 finalization was about 1.18 seconds. Denoising did not resolve the wording differences.
- Tail silence, a 300 ms prefix, and increased gain did not recover clip 03 with Unified 1120 ms.
- Prefixes of 1120, 2240, and 4000 ms each recovered the word.
  This identifies sensitivity to the initial model window. It does not establish a general accuracy improvement.
- Unified 560 ms with four threads passed the phrase checks but had 18.1 seconds of simulated p95 delay.

The final repeated run measured simulated p95 finalization of 824, 844, and 862 ms.
Its median processing-to-audio ratio was 0.661. Binary, model, and corpus hashes are in `parakeet-1120-context2/summary.json`.
Diagnostic comparisons and the original failing results are retained in the private corpus.

Full word error rate for your recordings remains unmeasured. The speech clips need reviewed reference transcripts.
The two silent references do not justify reporting zero WER for the corpus.

## References and applied patterns

- [Sherpa NeMo decoder, v1.13.8](https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.8/sherpa-onnx/csrc/online-recognizer-transducer-nemo-impl.h)
  provides the complete-window readiness condition behind the tail defect.
- [Sherpa Parakeet Unified decoder, v1.13.8](https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.8/sherpa-onnx/csrc/online-recognizer-transducer-nemo-parakeet-unified-impl.h)
  supplies its own EOF window padding and supports greedy decoding.
- [OpenWhispr Parakeet server](https://github.com/OpenWhispr/openwhispr/blob/85b01157f597036ad88c4901cea8d71ab8e46a43/src/helpers/parakeetWsServer.js)
  uses explicit end padding and bounds completion waits. This supported testing decoder-tail handling separately from capture.
- [VocaLinux recognition manager](https://github.com/VocaHQ/vocalinux/blob/96e012d9bd4e6d34d53cbf696dfbcce85fd492dc/src/vocalinux/speech_recognition/recognition_manager.py)
  joins capture and drains queued audio at stop. This informed the finalization and recovery checks.
- [Handy clipboard delivery](https://github.com/cjpais/Handy/blob/73ab851c2b6242283759a4c101b60f0ece132f08/src-tauri/src/clipboard.rs)
  separates clipboard handling and paste behavior. This informed bounded delivery and recovery review.

These source comparisons do not measure competitor accuracy or prove parity.

## Release gates

The repeatable system is described in [reliability checks](../reliability.md).
It rejects changed audio, missing expectations, omitted phrases, silent hallucinations, unstable runs, and threshold failures.
It retains raw and formatted hypotheses so recognition and formatting errors can be separated.

Before calling this production-ready:

1. Review reference transcripts and test a separate holdout corpus.
2. Review remaining substitutions and endings, including clip 05, against the audio.
3. Verify microphone boundaries and actual insertion in daily target applications.
4. Test suspend, device removal, portal denial, and compositor restart.
5. Compare competitors on identical reviewed audio and hardware.

Unit tests, mocks, and offline replay cannot replace these checks.

## Verification results

- `bash scripts/check.sh` passed formatting, Clippy with warnings denied, 166 Rust tests, and three Python tests.
- All three opt-in native tests passed with the selected Parakeet model.
- The private nine-clip corpus passed three sequential runs with identical text.
- Five unrelated opt-in library tests were not part of the ordinary test run.
- CUDA and live target-application insertion remain unverified by this audit.

The three reference-labelled samples supplied in the Nemotron model archive also passed a separate 5% WER gate.
Parakeet produced three word edits across 77 reference words, or 3.90% WER.
The differences include British/American spelling and `for ever` versus `forever`.
This small read-speech check is separate from your unscored dictations and cannot estimate daily accuracy.
Its simulated p95 finalization was 627 ms. The private report is `public-check.json`.

## Installed configuration

The measured CPU binary was installed at `~/.local/bin/nvstt`. Its SHA-256 matches the repeated-run report.
The active configuration selects Parakeet Unified 1120 ms, four threads, no denoising, and no speech gate.
Conservative number formatting is enabled.
The existing desktop autostart service is active, and `nvstt status --json` reports `idle`, `ready`, and `model_ready: true`.
A systemd drop-in restarts process failures with a bounded retry rate.

The previous binary is saved at `~/.local/state/nvstt/backups/20261004T074712Z/nvstt.previous`.
No configuration or restart drop-in existed before this installation. The backup manifest records the changed paths.
The original seven retained recordings and transcript history remain available.
