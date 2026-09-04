# nvstt implementation specification

Status: accepted direction; initial implementation is complete. Host-specific
desktop matrix verification remains an operational follow-up.

`nvstt` is a local Linux voice-dictation daemon with a CLI client. It is
Wayland-first and uses native desktop permission paths before fallbacks.

## Product contract

- `nvstt toggle` starts listening when the daemon is idle.
- `nvstt toggle` stops listening, finalizes recognition, and delivers one final
  transcript when the daemon is listening.
- Interim streaming text is never inserted into the focused client.
- A transcript enters history when transcription succeeds, even when delivery
  fails.
- History persists the last ten transcripts and metadata. Audio is not retained
  by default.
- Transcription is local. The first model is
  `nvidia/parakeet-unified-en-0.6b` through an INT8 sherpa-onnx buffered
  streaming export. The initial profile is 560 ms latency.
- Delivery status is separate from transcription status.
- Notifications are passive and must not steal focus.

## Runtime shape

Use one Rust binary with a daemon mode and CLI commands. The daemon owns all
mutable state. CLI commands use a private Unix socket under
`$XDG_RUNTIME_DIR/nvstt.sock`.

```text
nvstt CLI
   |
   | private Unix socket
   v
voice daemon
   |-- Recorder       (cpal, PipeWire-compatible default source)
   |-- Recognizer     (sherpa-onnx OnlineRecognizer)
   |-- TextSink       (portal/libei, virtual keyboard, clipboard, uinput)
   |-- HistoryStore   (bounded JSON history)
   `-- Notifier       (desktop notifications)
```

Recommended service behavior:

- `nvstt daemon` runs the long-lived service.
- `nvstt toggle`, `status`, and `history` connect to it.
- The user service is installed and started through systemd; the CLI reports a
  connection error when no daemon is running.
- The service must not require root.

## State machine

```text
starting -> idle -> listening -> finalizing -> delivering -> idle
              |         |            |             |
              `---------`------------`-------------`--> error -> idle
```

States:

- `starting`: load configuration, model, recorder, and capability probes.
- `idle`: ready for a toggle.
- `listening`: capture audio and feed recognizer chunks.
- `finalizing`: close the recognizer stream and obtain final text.
- `delivering`: run the native-first text sink.
- `error`: expose a stable error and return to `idle` when safe.

Rules:

- A toggle during `finalizing` or `delivering` returns `busy`.
- `cancel` is valid during `listening` and `finalizing`; it produces no history.
- Empty or whitespace-only final text is a transcription failure.
- A successful transcript is saved before delivery completes.
- The daemon never runs two recognizers at once.

## Model and recognition

Use the current Parakeet Unified model:

```text
Model family: nvidia/parakeet-unified-en-0.6b
Runtime:      sherpa-onnx OnlineRecognizer
Artifact:     sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms
Language:     English
Provider:     CPU by default; CUDA after a successful host preflight
```

Keep the recognizer warm. The recorder callback places bounded mono PCM in a
queue, and a recognition worker feeds chunks to the online recognizer while
the user is listening. Keep interim hypotheses inside the recognizer. On stop,
drain the queue, mark end-of-input, drain the final hypotheses, reset the
stream, and pass one final transcript onward.

Store the model archive under `$XDG_DATA_HOME/nvstt/models/`. Verify its
required file set and expose model readiness through `nvstt status`. The
explicit `nvstt model install` command downloads the pinned archive into a
private staging directory and activates it only after extraction succeeds.

## Audio capture

Use `cpal` for the first capture implementation. Prefer the desktop default
source. Convert input to mono floating-point PCM at the recorder boundary. The
audio callback must not perform model inference or blocking I/O. The worker
resamples the bounded queue to 16 kHz before recognition. It uses FFT
resampling. Optional RNNoise processing runs in the worker at 48 kHz before
the final 16 kHz conversion.

Do not add VAD or automatic stop in the first slice. Manual toggle defines the
session boundary. Add VAD later as an optional mode.

## Text delivery

Expose one interface:

```text
probe() -> capabilities and reason
authorize_if_needed() -> permission result
send_final_text(text) -> delivery result
```

Probe at runtime. Do not choose a backend from the compositor name alone.

Priority:

1. XDG RemoteDesktop portal with keyboard-only permission and libei/EIS.
2. `zwp_virtual_keyboard_v1` where the compositor exposes and authorizes it.
3. A configured compositor adapter when one is required.
4. Clipboard copy, with optional paste only when an authorized key path exists.
5. `uinput`/`ydotool` only as an explicit future opt-in.

Request keyboard permission lazily on the first automatic delivery. The Rust
delivery layer uses `eitype` for the portal/libei handshake, keeps the EI
session alive for later final transcripts, and caches the portal restore token
under the user state directory with mode `0600`. If the portal is denied or
unavailable, it waits at most ten seconds and probes `zwp_virtual_keyboard_v1`
through `wrtype` for wlroots/Smithay compositors. If both native paths fail,
it copies the transcript and includes the automatic-delivery failure reason in
the backend metadata. It never claims that clipboard copy typed the text.

Never show an overlay that can steal focus. Deliver to the focused client at
send time. Do not log transcript text, clipboard contents, or portal tokens.

## History

Use a JSON file at `$XDG_STATE_HOME/nvstt/history.json` for the first release.
The daemon serializes all writes and uses a temporary file plus rename.

Each record contains:

```text
id, created_at, duration_ms, model, transcript,
transcription_status, delivery_status, delivery_backend
```

Retain only the newest ten records. Keep records when transcription succeeds,
even if delivery fails. Do not store audio.

## Configuration

The initial file is `$XDG_CONFIG_HOME/nvstt/config.toml`:

```toml
model = "parakeet-unified-en-0.6b"
```

Keep user-facing configuration limited to model selection for now. Store the
resolved artifact, latency profile, and provider as runtime metadata.

## CLI contract

```text
nvstt toggle
nvstt cancel
nvstt status
nvstt status --json
nvstt history
nvstt history --limit 10 --json
nvstt model install
nvstt daemon
```

The CLI returns non-zero exit status for connection errors, invalid state
transitions, transcription failures, and an unsuccessful clipboard fallback.
A successful clipboard fallback returns success while its backend metadata
states that automatic delivery was unavailable. JSON output uses stable state
and result names.

## Notifications

Send passive notifications for:

- daemon initialized and ready;
- listening started;
- finalizing/transcribing;
- transcription succeeded;
- delivered successfully;
- copied to clipboard because delivery was unavailable;
- transcription failed;
- delivery failed;
- portal permission failures are included in the delivery result and the
  clipboard notification; no permission dialog is shown at daemon startup.

Notifications must not take keyboard focus.

## Security and privacy

- Keep the Unix socket mode `0600`.
- Keep configuration, history, model metadata, and restore tokens user-owned.
- Request keyboard access only. Never request pointer, touch, or screen capture.
- Do not require root for normal operation.
- Keep `uinput` disabled until the user explicitly enables it.
- Do not retain audio or write transcript contents to logs.
- Treat password and secure fields as compositor/application policy. Report
  delivery failure instead of bypassing that policy.

## Test plan

### Unit tests

- State transitions and invalid commands.
- Final-only delivery behavior.
- Empty transcript handling.
- History ordering, ten-record trimming, and atomic persistence.
- Separate transcription and delivery results.
- Notification mapping.

### Component tests

- Fake recorder with deterministic PCM.
- Fake streaming recognizer with interim and final hypotheses.
- Fake text sinks for delivered, copied, denied, and unsupported outcomes.
- Portal token replacement and revocation.

### Host tests

- Model load and warm streaming on CPU.
- 160/240/560 ms profiles and 2/4/8 inference threads.
- Optional CUDA provider without silent CPU fallback.
- PipeWire default source, DMIC, analog input, headset input, and device loss.

### Desktop matrix

Run the same tests on current GNOME/Mutter, KDE/KWin, Sway, Hyprland, and Niri.
Test native Wayland, XWayland, GTK, Qt, Electron, browser, terminal, password,
occupied clipboard, denied portal, revoked permission, screen lock, and
compositor restart cases.

## Delivery milestones

1. Rust workspace, CLI, daemon, socket, and state machine with fake components. (done)
2. Persisted history, notifications, configuration, and status JSON. (done)
3. PipeWire-compatible capture and Parakeet Unified streaming on CPU. (done;
   host microphone verification remains)
4. Clipboard delivery and accurate result reporting. (done)
5. Portal/libei delivery on GNOME and KDE. (implemented; requires host portal
   consent and compositor verification)
6. Virtual-keyboard delivery on supported wlroots/Smithay compositors.
   (implemented; requires host compositor verification)
7. Packaging, service integration, and model file verification. (implemented;
   desktop matrix testing remains)
