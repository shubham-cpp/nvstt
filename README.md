# nvstt

Local Linux voice dictation for Wayland desktops.

## Current status

The repository contains a working native first release slice:

- Rust CLI and long-lived Unix-socket daemon.
- Toggle, cancel, status, and history commands.
- Explicit dictation state machine.
- Persisted ten-record JSON history.
- Separate transcription and delivery results.
- Passive notification boundary.
- Final-only delivery contract.
- CPAL default-input capture with mono downmix and sample-rate conversion.
- Live bounded audio handoff to the recognizer while listening.
- Native Nemotron streaming 0.6B INT8 recognizer through sherpa-onnx.
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

## Configuration

If the file does not exist, nvstt selects Nemotron streaming with its local
speech gate. Create `$XDG_CONFIG_HOME/nvstt/config.toml` (normally
`~/.config/nvstt/config.toml`) with:

```toml
[model]
name = "nemotron-speech-streaming-en-0.6b"
streaming_profile = "560ms"
speech_gate = true

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

The five-second queue capacity is provisional. It does not guarantee a
five-second recording limit or any particular insertion latency. The separate
30-minute session limit stays unchanged.

Cleanup removes clear lowercase or title-case `uh` and `um` variants.
Ambiguous words, uppercase acronyms, and technical symbols remain.

Inverse text normalization is on by default. It converts spoken numbers,
dates, money, and measurements to written forms. Set `text.itn = false` to
keep the spoken forms. The phrase `give me a second` keeps the word `second`.

Inverse text normalization runs before `[text.replacements]`. Patterns see
normalized text when `text.itn = true`. Replacement values receive no later
normalization. Existing replacement patterns may need adjustment.

Multiword replacement rules cannot consume interior punctuation-only tokens,
even when the configured pattern includes them. Single-token punctuation
mappings remain allowed. For example, `"nv , stt" = "nvstt"` does not match
`nv , stt`, but `"," = "comma"` can replace a standalone comma.

ITN can still change literal technical words. For example, `DOT` becomes `.`.
Set `text.itn = false` when literal technical wording matters more than
spoken-number and punctuation conversion. This repair does not change your
configuration automatically.

See [ADR 0013](docs/adr/0013-dictation-integrity.md) for compatibility changes
and validation limits. The public CLI, IPC, and configuration schemas stay
unchanged, as does the history format.

The closed registry supports Nemotron profiles 80 ms, 160 ms, 560 ms, and
1120 ms. It also supports Parakeet Unified profiles 240 ms, 560 ms, and
1120 ms for rollback. Existing Parakeet configuration does not enable VAD
unless it sets `speech_gate = true`.

## Model files

The selected model directory contains its encoder, decoder, joiner, token
files, and, when enabled, `silero_vad.onnx`. A new default install selects:

```text
$XDG_DATA_HOME/nvstt/models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25/
```

The directory must contain `encoder.int8.onnx`, `decoder.int8.onnx`,
`joiner.int8.onnx`, `tokens.txt`, and `silero_vad.onnx`. If `XDG_DATA_HOME`
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
capture. It keeps the seven newest committed recordings. Text history keeps ten
successful transcripts, so its entries need not match the audio list. Match a
successful history entry to audio by its session ID in `metadata.json`.

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

On Linux, install the CPAL and Wayland/XKB build headers first. Fedora uses
`alsa-lib-devel`, `wayland-devel`, and `libxkbcommon-devel`; Debian and Ubuntu
use `libasound2-dev`, `libwayland-dev`, and `libxkbcommon-dev`.

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
nvstt model install         # explicitly download the selected model and VAD
nvstt model status [--json] # local model file check
nvstt model path             # expected model directory
nvstt model evaluate --manifest PATH --json # private-corpus report
```

See [IMPLEMENTATION_SPEC.md](./IMPLEMENTATION_SPEC.md) for the accepted
architecture and milestone plan.
