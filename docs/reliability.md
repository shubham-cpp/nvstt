# Dictation reliability checks

Use the same private audio and reviewed text for each comparison. Never use a model's output as verified reference text.

## Save a fixed corpus

```sh
python3 scripts/reliability.py snapshot --output "$HOME/.local/state/nvstt/private-evaluation/review"
```

The command copies the last seven recordings. It records SHA-256 hashes and creates a private JSONL manifest.
It never changes the live recordings or their retention policy. The output directory must not exist.

Edit the manifest. Each line accepts these fields:

```json
{"id":"short","audio":"short.wav","category":"dictation","raw_reference":"I have twenty one apples","reference":"I have 21 apples","expected_prefix":"I have","expected_suffix":"21 apples","required_phrases":["21"],"audio_sha256":"hash-from-snapshot"}
```

- `reference` is the intended final text, including number formatting and configured corrections.
- `raw_reference` is the spoken text used to score raw recognition separately.
- Omit a reference or use `null` until a person checks it against the audio.
- An empty reference means no text should appear. It does not mean "unknown".
- Prefix, suffix, and phrase checks catch omissions without a complete reference.
- `silence`, `noise`, and `no_speech` categories require empty output.
- `noisy_speech` is ordinary speech. Noise in that label does not make it a silent clip.

Keep ordinary dictation, quiet speech, short utterances, names, technical terms, numbers, pauses, and stop boundaries in the corpus.
Keep a separate holdout corpus. Do not tune replacements to the test sentences.

## Run and compare

Build the release binary. Debug builds do not provide useful timing evidence.

Run `bash scripts/check.sh` for formatting, Clippy, Rust tests, and runner tests.
This check needs no private speech or installed ASR model.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
python3 scripts/reliability.py run \
  --manifest "$HOME/.local/state/nvstt/private-evaluation/review/manifest.jsonl" \
  --output /tmp/nvstt-baseline \
  --model parakeet-unified-en-0.6b --profile 1120ms --threads 4 --repeats 3
```

Use Fedora Distrobox for builds on a host without ALSA development files:

```sh
distrobox enter --name Fedora -- cargo test
```

The runner verifies audio hashes, runs evaluations in sequence, and saves private JSON reports.
It records binary, model, and corpus hashes, platform, complete configuration, and repeated-run differences.
The default simulated p95 finalization limit is 2000 ms. Change it with `--max-finalization-ms`.
Each run has a ten-minute deadline. `--timeout` changes it.
No evaluation types text, records microphone input, or changes history.

For a fully reviewed corpus, add `--max-wer 0.05 --max-finalization-ms 2000`.
These are example acceptance limits. Agree on limits before comparing candidates.
The WER limit requires references for every clip and at least one reference word.
A corpus with only phrase checks cannot establish a word error rate.

```sh
python3 scripts/reliability.py compare \
  /tmp/nvstt-baseline/summary.json /tmp/nvstt-candidate/summary.json \
  --require-references
```

Comparison rejects changed corpora, failed runs, unstable text, new word errors, and excessive latency growth.
The default latency allowance is 20 percent. Use `--latency-ratio` to change it before testing.
Raw reports retain substitutions, deletions, insertions, raw text, final text, and per-clip failures.

Direct evaluation also supports checks:

```sh
nvstt model evaluate --manifest /private/manifest.jsonl \
  --model parakeet-unified-en-0.6b --streaming-profile 1120ms \
  --threads 4 --denoise false --itn true --check --json
```

## Native regression tests

Unit tests do not load ASR models. The optional tests in `tests/native_reliability.rs` do.

Set `NVSTT_REPLAY_MANIFEST` to a private manifest.
Set `NVSTT_REPLAY_EXPECTATIONS` to a JSON array such as:

```json
[{"id":"short","contains":["21 apples"]}]
```

Select the installed model with `NVSTT_REPLAY_MODEL` and `NVSTT_REPLAY_PROFILE`.
Set `NVSTT_REPLAY_GATE=true` to test session speech detection.
The sample-conservation test also needs `NVSTT_VAD_MODEL` and `NVSTT_REPLAY_WAV`.

```sh
cargo test --release --test native_reliability -- --ignored
```

The tests cover required words, exact sample preservation, partial detector frames, silence, cancellation, and repeated sessions.
Private speech stays outside Git.

## Recovery and corrections

```sh
nvstt transcribe /private/saved/audio.wav --model parakeet-unified-en-0.6b --streaming-profile 1120ms --json
```

This command prints raw and final text. It does not type recovered text into a possibly different focused window.

Use `nvstt recover --json` to select the newest complete retained recording automatically.
The output includes the source session, capture status, audio path, model, and profile.
Failed transcription attempts remain eligible for recovery.
Uncommitted recordings are skipped. Recovery does not alter history or retention.

### Check personal corrections

`nvstt vocabulary test "your phrase"` previews the same cleanup used for dictation.
It needs no daemon or model. Rules reload between recordings and remain fixed during each recording.
`model evaluate`, `transcribe`, and `recover` also load personal corrections.
Evaluation reports include the merged replacement rules in their configuration snapshot.
Keep those snapshots when comparing results: a rule change can change final-text scores without changing raw recognition.

Tests cover rule precedence, non-cascading replacement, number formatting, atomic writes, and concurrent writer rejection.
Daemon tests cover edits during recording, rule removal, and corrupt-file rejection before capture.

Use `[text.replacements]` for repeatable corrections to names and technical terms.
Number formatting runs first. Replacement values receive no further formatting.
The number formatter handles numeric spans. It preserves ranges, technical words, and ordinary conjunctions.
It does not guess dates or times from ambiguous phrases. Set `text.itn = false` for literal output.

## Limits of the evidence

The finalization metric simulates audio arrival in 20 ms chunks. It includes decoder backlog and model flush time.
It excludes microphone delivery delay, model startup, storage, portal authorization, and the target application's processing time.
Do not call it measured hotkey-to-insertion latency.

Replay cannot recover words absent from the recorded waveform. It cannot verify focus or text received by another application.
Before a production release, test real microphone start/stop boundaries and insertion in the actual editors, terminals, and browsers.
Repeat those checks after suspend, microphone changes, permission denial, and compositor restart.
Use the same reviewed corpus and hardware for competitor comparisons. Source review alone cannot establish accuracy parity.

Parakeet TDT v3 is an optional full-utterance model. It rejects audio over 120 seconds before native inference.
Use Parakeet Unified for longer dictations. Failed attempts still retain captured audio under the seven-record policy.

## Native delivery regression

Build the test sender, then run a private compositor check:

```sh
cargo build --locked --example delivery_probe
python3 scripts/check_delivery.py --target gtk --unicode --repeats 100
python3 scripts/check_delivery.py --target kitty --unicode --repeats 100
python3 scripts/check_delivery.py --target codex --repeats 100
```

These checks need KWin, `dbus-run-session`, `wl-copy`, PyGObject, and dbus-python.
Terminal checks also need Kitty. The Codex check needs an installed, configured Codex CLI.
It opens an isolated composer and never submits the fixture.
Private artifacts include the input, received text or screen, process logs, and `result.json`.

Reproduce the former stall with:

```sh
python3 scripts/check_delivery.py --target codex --legacy --repeats 20
```

The legacy run should fail on the affected Codex/Kitty combination.
It records unread terminal bytes before sending one diagnostic space.
The normal run requires zero unread bytes and a single complete paste marker.
GTK and raw Kitty checks require exact text equality.
See [ADR 0016](adr/0016-deliver-transcripts-as-paste.md) for the diagnosis and delivery limits.

## Daemon recovery

For desktops using the generated `app-nvstt@autostart.service`, a bounded restart drop-in is available.
Copy `contrib/nvstt-autostart-restart.conf` to
`~/.config/systemd/user/app-nvstt@autostart.service.d/restart.conf`, then run `systemctl --user daemon-reload`.
It retries process failures after two seconds and limits starts to three per minute.
It does not restart a manually stopped daemon or resume microphone capture.
It cannot restore audio lost in a process crash.
