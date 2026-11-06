# nvstt

This project is archived.

Use [Hex](https://github.com/anomalyco/hex/releases/) for local voice dictation on Linux, macOS, and Windows.

Hex holds a shortcut, transcribes on the device, and pastes into the current app.

The text below describes nvstt at the time of archive.

Local Linux voice dictation for Wayland desktops.

## Current status

The repository contains a working native first release slice:

- Rust CLI and long-lived Unix-socket daemon.
- Toggle, cancel, status, and history commands.
- Explicit dictation state machine.
- Persisted ten-record JSON history.
- Separate transcription and delivery results.
- Generic Linux system tray with five activity icons and notification fallback.
- Final-only delivery contract.
- Runtime microphone selection, mono downmix, and sample-rate conversion.
- Live bounded audio handoff to the recognizer while listening.
- Native Parakeet and Nemotron INT8 recognizers through sherpa-onnx.
- Local Silero VAD gate with final-only delivery and no-speech results.
- Native-first text delivery through the XDG RemoteDesktop portal/libei path.
- Runtime-probed `zwp_virtual_keyboard_v1` fallback for wlroots/Smithay WMs.
- Clipboard fallback with the native-delivery failure reason in the result.

The first automatic delivery request can show the desktop keyboard-permission
dialog. The daemon waits up to ten seconds for portal authorization, then
continues with the next backend if the portal is unavailable or does not
respond. It stores a user-only portal restore token under its state directory.
If the portal is unavailable or denied, it probes the compositor's
virtual-keyboard global before falling back to `wl-copy`.

GNOME and KDE need a working `xdg-desktop-portal` RemoteDesktop backend for
the portal path. Sway, Hyprland, Niri, and other wlroots/Smithay sessions can
use the direct virtual-keyboard path when their compositor advertises it.

The current daemon uses a development recognizer only when
`NVSTT_DEV_TRANSCRIPT` is set. Without that variable, the model files must be
installed before listening can start. `nvstt status` reports model readiness;
`nvstt model status` checks the files without contacting the daemon.

## System tray and microphones

The daemon publishes one StatusNotifierItem and a D-Bus menu through ksni.
It uses no KDE library. Plasma, Waybar, and Quickshell can host this protocol.
GNOME needs a tray extension.

The icon shows ready, listening, transcribing, typing, or error.
Starting uses the ready shape with a `Starting` label.
Typing covers the delivery operation, including portal authorization.
The last result remains visible after the daemon returns to idle.
Clipboard fallback reports `Copied`, so you know to paste the text.

Click the item to open its menu. Panel behavior can differ.
The microphone submenu includes system default, physical inputs, and virtual inputs.
Playback monitors have a separate label.
Use `Refresh inputs` after connecting or removing a device.
Input changes apply between dictations. Selection resets when the daemon restarts.
A missing selected device fails explicitly. nvstt never substitutes another input.

```bash
nvstt audio inputs --json
nvstt audio select --id 'pulseaudio:SOURCE_NAME'
nvstt audio select --system-default
nvstt clear-error
nvstt daemon --no-tray
```

Copy the input ID from `audio inputs`. Display names are not selection IDs.
System default resolves again at each recording start.
Selection changes nvstt only. It does not change the system microphone default.

Cancel recording is available while listening.
Use your existing toggle shortcut to stop and deliver text.
The menu has no delivery action because opening it can change keyboard focus.

Status remains available during transcription and delivery.
Other requests can return `busy` during an accepted operation.
Retry those requests after completion. nvstt does not defer microphone changes.
A changed menu invalidates old item IDs, including cancellation from an earlier recording.

With a reachable tray host, nvstt suppresses routine phase notifications.
Failures and clipboard feedback still produce notifications.
Without a host, or with `--no-tray`, normal phase notifications remain.
A panel can hide an item despite reporting a host. Use `--no-tray` if you need notifications.
Tooltips and menu labels exclude transcripts, portal tokens, and raw backend errors.

Waybar needs `"tray"` in its configured module list.
Quickshell needs a `SystemTray` host and menu handling through `QsMenuAnchor`.
Its icon-only examples can require additional menu wiring.
No panel configuration changes are part of nvstt installation.

Live verification covered Plasma registration, ready/listening rendering, and microphone selection.
Three actual sources opened and canceled, including Easy Effects.
D-Bus tests cover stale actions, absent hosts, late hosts, and watcher restart.
Waybar and Quickshell live rendering remain untested in this workspace.
Physical unplug, suspend/resume, and whole session-bus replacement also remain untested.

## Running from Fedora Distrobox

Build through the existing Fedora container:

```bash
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo build --locked --release
```

An init-enabled container can have its own session bus.
Its tray must connect to the desktop host's session bus.
Keep `XDG_RUNTIME_DIR` unchanged when fixing this connection.

Run the host launcher with a container binary path:

```bash
NVSTT_BINARY="$PWD/target/release/nvstt" scripts/run_in_distrobox.sh daemon
```

The launcher maps the host Unix bus socket through Distrobox's `/run/host` mount.
It fails if that mapping is absent. It does not silently use the container bus.
It forwards shutdown to its own container process and waits for exit.
If that process ignores shutdown for 30 seconds, the launcher stops it forcibly.
It verifies the launch token before each signal.
`NVSTT_DISTROBOX` defaults to `Fedora`. Set it for a different container.
The normal shared home and audio/display integration must remain available.

For a host-managed service, install the launcher as `~/.local/bin/nvstt-distrobox`.
Use [the Distrobox unit](contrib/nvstt-distrobox.service) instead of the native unit.
Install the binary inside Fedora at the path set by `NVSTT_BINARY`.
Start the service within a graphical user session.
Run only one daemon for the normal socket.
Stop an existing service or autostart process before changing launch arrangements.

The launcher requires host Bash, Distrobox, and `gio` for temporary-file cleanup.
ksni and the PulseAudio protocol client need no GTK, Qt, or native libpulse headers.
The existing ALSA, Wayland, XKB, and C++ build requirements still apply.
Capture now prefers CPAL's PulseAudio backend when its server is available.
It uses the PipeWire PulseAudio compatibility socket on typical PipeWire systems.

`--no-tray` changes presentation only. It does not restore the old audio backend.
Keep your previous binary and launch command before deploying a replacement.
Verification used separate binaries, sockets, and state directories.
It did not replace or restart the active daemon.
The tray changes passed CPU validation. CUDA validation remains pending.
For the later reliability repairs and installed configuration, see the
[dictation reliability audit](docs/research/18-dictation-reliability-audit.md).

## Configuration

If the file does not exist, nvstt selects Parakeet Unified with its 1120 ms profile. The optional
speech detector preserves the full waveform. Create `$XDG_CONFIG_HOME/nvstt/config.toml` (normally
`~/.config/nvstt/config.toml`) with:

```toml
[model]
name = "parakeet-unified-en-0.6b"
streaming_profile = "1120ms"
speech_gate = false
num_threads = 4

[audio]
denoise = false

[text]
itn = true

[text.replacements]
"nv stt" = "nvstt"
parakeet = "Parakeet"
```

The audio worker uses FFT resampling for all non-16 kHz input. Set
`audio.denoise = true` to enable RNNoise before recognition. Denoise is off by
default.

Capture uses a bounded queue. If the queue loses audio or the input backend
reports a stream error, nvstt rejects that session. It does not type or save
a partial transcript. Start another session after correcting the input issue.

Start speaking when the app shows Listening. Capture requests 20 ms buffers and confirms the first audio callback.
Normal stop keeps 200 ms of trailing audio before closing the microphone. Cancellation closes it without this tail.
See the [capture-boundary tests](docs/research/19-toggle-capture-boundaries.md) for measured startup times and diagnostics.

The five-second queue capacity is provisional. It does not guarantee a
five-second recording limit or any particular insertion latency. The separate
30-minute session limit stays unchanged.

Cleanup removes clear lowercase or title-case `uh` and `um` variants.
Ambiguous words, uppercase acronyms, and technical symbols remain.

Inverse text normalization is on by default. It converts spoken numeric spans
to written numbers. It preserves ranges and technical words. Set `text.itn = false` to
keep the spoken forms. The phrase `give me a second` keeps the word `second`.

Inverse text normalization runs before `[text.replacements]`. Patterns see
normalized text when `text.itn = true`. Replacement values receive no later
normalization. Existing replacement patterns may need adjustment.

Multiword replacement rules cannot consume interior punctuation-only tokens,
even when the configured pattern includes them. Single-token punctuation
mappings remain allowed. For example, `"nv , stt" = "nvstt"` does not match
`nv , stt`, but `"," = "comma"` can replace a standalone comma.

`five to eight` becomes `5 to 8`, and `DOT` remains `DOT`.
Times and dates are not inferred from ambiguous wording.
Set `text.itn = false` to disable numeric conversion.
Incorrect setting types and duplicate flat/section settings are errors.

See [ADR 0013](docs/adr/0013-dictation-integrity.md) for compatibility changes
and validation limits. The tray adds CLI and IPC commands and status fields.
Existing commands, configuration, and the history format remain compatible.

The closed registry supports Nemotron profiles 80 ms, 160 ms, 560 ms, and
1120 ms. It also supports Parakeet Unified profiles 240 ms, 560 ms, and
1120 ms. Parakeet TDT v3 uses the `offline` profile and a 120-second limit.
Set `speech_gate = true` to classify silent sessions without removing audio.
The default leaves this optional detector off.

## Model files

The selected model directory contains its encoder, decoder, joiner, token
files, and, when enabled, `silero_vad.onnx`. A new default install selects:

```text
$XDG_DATA_HOME/nvstt/models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-1120ms/
```

The directory must contain `encoder.int8.onnx`, `decoder.int8.onnx`,
`joiner.int8.onnx`, and `tokens.txt`. Speech detection also needs `silero_vad.onnx`. If `XDG_DATA_HOME`
is not set, use `~/.local/share/nvstt/models/`.

Install the model explicitly with:

```bash
nvstt model install
```

The command downloads the pinned official sherpa-onnx archive only when you
call it. It validates all required ASR and VAD files, then activates both
only after extraction succeeds. An incomplete existing installation is
replaced atomically. Network failures leave the active model directory
unchanged.

Install a candidate without changing the active configuration:

```bash
nvstt model install --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms
nvstt model status --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --json
```

## Recent dictation audio

After each stopped dictation, nvstt saves the original mono float32 input as
`audio.wav`, with `metadata.json`, in a private directory. This includes failed
and no-speech attempts, but not canceled sessions or sessions that never started
capture. It normally keeps the seven newest owned recordings. A published save
can temporarily leave more than seven if post-publication root sync or pruning
fails. Startup reconciliation or a later successful save restores the seven-entry
limit. Text history keeps ten successful transcripts, so its entries need not
match the audio list. Match a successful history entry to audio by its session ID
in `metadata.json`.

Find a WAV file with:

```bash
if [ -n "${XDG_STATE_HOME:-}" ]; then
  recordings="$XDG_STATE_HOME/nvstt/recordings"
else
  recordings="$HOME/.local/state/nvstt/recordings"  # ~/.local/state/nvstt/recordings
fi
find "$recordings" -mindepth 2 -maxdepth 2 -type f -name audio.wav \
  ! -path "$recordings/.staging-*/audio.wav" -print |
  while IFS= read -r wav; do
    [ -f "${wav%/*}/metadata.json" ] && printf '%s\n' "$wav"
  done
```

The shell list gives candidate WAV paths, not proof of committed recordings.
It checks only that `metadata.json` exists; it does not check its contents.
Select `audio.wav` only after you check its session metadata. Other WAV files
are not necessarily nvstt recordings.
The `capture` fields in the metadata mark partial recordings: nonzero
`dropped_samples` or a true `backend_failed`, `duration_exceeded`,
`stop_failed`, or `drain_failed` means audio can be missing. There is no
separate `partial` key. Recording directories use mode `0700`; `audio.wav`
and `metadata.json` use mode `0600`. These files are not encrypted. Backups
and other processes with account access can copy them.
Review and remove saved audio when you no longer need it.

Use a selected WAV in a private evaluation manifest. This example uses the
normal state-directory fallback; change the path if `XDG_STATE_HOME` is set:

```json
{"id":"boundary-1","audio":"/home/alex/.local/state/nvstt/recordings/1700000000000-1700000000000-1/audio.wav","reference":"hello there","category":"boundary"}
```

`model evaluate` bypasses live microphone capture. It cannot recover speech
before the first microphone callback or after stop. Saved WAVs can aid diagnosis,
but they do not fix missing words or reproduce capture timing and queue loss.

## Evaluate a model with a private corpus

Use `nvstt model evaluate` for an accuracy and finalization report. The JSONL
manifest stays private. Relative WAV paths resolve from its directory:

```json
{"id":"short-1","audio":"audio/short-1.wav","reference":"example text","category":"short_dictation"}
```

```bash
nvstt model evaluate --manifest /private/corpus/manifest.jsonl \
  --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --json
```

The report includes each hypothesis, S/D/I counts, WER, silent-clip failures,
and finalization latency. It never delivers text or changes configuration.

For a paired gate comparison, put one private JSONL manifest with corrected
references in the private state directory below. Use the same audio, Nemotron
560 ms model, and profile for both runs. Change only `--speech-gate`. Run these
commands only with your consent; keep the manifest, audio, and reports local.
Do not commit or upload them.

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

Compare each clip's reference and hypothesis, substitutions, deletions,
insertions, and silent-clip failures across the two reports. Review corrected
references to count first- and last-word errors yourself; the evaluator does
not calculate these counts. Compare finalization times separately. Replay
cannot measure live microphone callback timing, queue loss, or desktop text
insertion. Paired accuracy evaluation is pending a consented, corrected private
corpus. These instructions do not establish an accuracy improvement.

## Verify a Parakeet model with a WAV file

The repository includes a small Parakeet verification example. It reads a
16-bit PCM WAV file. It does not require a microphone, daemon, or Wayland
session:

```bash
cargo run --example transcribe_wav -- \
  --model-dir "$model_root/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms" \
  path/to/sample.wav
```

Use `--seconds N` to test only the first part of a long recording. The
provided `dist/` sample is an MP3, so decode it to 16 kHz mono PCM first. The
reference text is in the matching `.txt` file in that directory. The example
is a verification aid and does not add audio or model files to the build.

The supplied Independence Day sample was decoded to 16 kHz mono and completed
on CPU with this recognizer. The opening matched the reference text; the full
spoken content was recovered with normal ASR substitutions such as `Sahil` →
`Sahi` and `Jai Bharat` → `Jai Baharad`.

## Install and run as a user service

Build and install the binary, then install the user unit:

```bash
cargo install --path . --locked
mkdir -p "$HOME/.config/systemd/user"
install -m 0644 contrib/nvstt.service \
  "$HOME/.config/systemd/user/nvstt.service"
systemctl --user daemon-reload
systemctl --user enable --now nvstt.service
systemctl --user status nvstt.service
```

The unit runs without root and uses the normal user XDG directories. Keep the
user service active in a graphical login. To stop it, run
`systemctl --user disable --now nvstt.service`.

For a one-off foreground run, use `nvstt daemon` instead. The CLI commands
connect to the user daemon over a private Unix socket.

## Development run

On Linux, install the CPAL and Wayland/XKB build headers first. The default
CPU build also links the C++ runtime for the static sherpa-onnx libraries.
Fedora uses `gcc-c++`, `pkg-config`, `alsa-lib-devel`, `wayland-devel`, and
`libxkbcommon-devel`. Debian and Ubuntu use `g++`, `pkg-config`,
`libasound2-dev`, `libwayland-dev`, and `libxkbcommon-dev`. On Fedora 43 the
`libstdc++.so` linker file comes from `gcc-c++`, not from `libstdc++-devel`.

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- --help
cargo run -- model status
```

For a local state-machine smoke test, run the daemon with a development
transcript in one terminal:

```bash
NVSTT_DEV_TRANSCRIPT="hello from nvstt" cargo run -- daemon
```

Then call `cargo run -- toggle` twice from another terminal. The first final
delivery attempts the portal and may ask for keyboard permission. On a
wlroots/Smithay compositor that exposes `zwp_virtual_keyboard_v1`, the daemon
can type arbitrary UTF-8 through a transient XKB keymap. If neither native
path is available, it copies the text with `wl-copy` and reports why automatic
delivery was not possible.

The main commands are:

```text
nvstt toggle                 # start, then stop and deliver
nvstt cancel                 # cancel an active session
nvstt status [--json]        # daemon and model readiness
nvstt history [--json]      # last ten successful transcripts
nvstt audio inputs --json   # refresh microphone IDs and source kinds
nvstt audio select --id ID  # select input for the next recording
nvstt audio select --system-default
nvstt clear-error
nvstt daemon --no-tray      # keep notification-only presentation
nvstt model install         # explicitly download the selected model and VAD
nvstt model status [--json] # local model file check
nvstt model path             # expected model directory
nvstt model evaluate --manifest PATH --json # private-corpus report
```

See [IMPLEMENTATION_SPEC.md](./IMPLEMENTATION_SPEC.md) for the accepted
architecture and milestone plan.

## Reliability and recovery

Final transcripts are pasted as one operation through the authorized native keyboard.
nvstt sets CLIPBOARD and PRIMARY, then sends Shift+Insert.
This avoids long streams of simulated keystrokes and supports Unicode outside the keyboard layout.
Both clipboard selections contain the latest transcript afterward.
The focused field must support Shift+Insert. If automatic delivery fails, paste from the clipboard or use history.

See [reliability checks](docs/reliability.md) for private corpus snapshots,
repeatable model comparisons, release limits, and native regressions.
The reports separate raw recognition from final text. Unknown references remain unscored.

Recover saved audio without typing into the desktop:

```sh
nvstt recover
nvstt recover --json
nvstt transcribe /private/audio.wav --json
```

`recover` selects the newest complete saved recording, including failed transcription attempts.
It uses your current model and corrections. It leaves history and saved audio unchanged.
It cannot restore speech absent from the audio.

### Personal corrections

Teach recurring spelling corrections without restarting the daemon:

```sh
nvstt vocabulary add "para keet" "Parakeet"
nvstt vocabulary add "nv stt" "nvstt"
nvstt vocabulary test "Use para keet with twenty one files in nv stt."
nvstt vocabulary list
nvstt vocabulary remove "para keet"
```

The preview prints `Use Parakeet with 21 files in nvstt.`
Corrections apply after number formatting. They match whole phrases and ignore ASCII letter case.
Longer phrases win. Personal rules override matching rules in `config.toml`.
Replacement text does not trigger another correction.
Use the preview to check a rule before dictating.

Edits apply at the start of the next dictation. Active recordings keep their original rules.
Rules live in `$XDG_CONFIG_HOME/nvstt/vocabulary.json`, normally `~/.config/nvstt/vocabulary.json`.
Writes are private and atomic. Concurrent edits fail clearly instead of overwriting another edit.
Invalid files stop a new dictation with an error.
These are text corrections; they do not retrain the speech model.

History writes use private atomic files. Clipboard tools have a deadline.
After a partial native typing failure, nvstt copies the complete text instead of typing a second copy.
A failed native connection can reconnect on the next dictation.

See [ADR 0015](docs/adr/0015-preserve-complete-audio-and-measure-recognition.md)
for the audio-preservation and conservative-number decisions.
