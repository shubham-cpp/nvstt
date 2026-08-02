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
- Parakeet Unified 0.6B INT8 streaming recognizer through sherpa-onnx.
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

The only user setting is the model name. If the file does not exist, nvstt
uses Parakeet Unified by default. Create
`$XDG_CONFIG_HOME/nvstt/config.toml` (normally
`~/.config/nvstt/config.toml`) with:

```toml
model = "parakeet-unified-en-0.6b"
```

The loader also accepts the older `[model] name = "..."` form. Parakeet
Unified is the only supported model in this release; another value produces a
clear configuration error.

## Model files

Place the extracted
`sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms` archive at:

```text
$XDG_DATA_HOME/nvstt/models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms/
```

The directory must contain `encoder.int8.onnx`, `decoder.int8.onnx`,
`joiner.int8.onnx`, and `tokens.txt`. If `XDG_DATA_HOME` is not set, use
`~/.local/share/nvstt/models/`.

Install the model explicitly with:

```bash
nvstt model install
```

The command downloads the pinned official sherpa-onnx archive only when you
call it. It shows progress, validates the four required model files, and
activates the model only after extraction succeeds. An incomplete existing
installation is replaced atomically. Network failures leave the active model
directory unchanged.

## Verify a model with a WAV file

The repository includes a small verification example. It reads a 16-bit PCM
WAV file and runs the same Parakeet recognizer that the daemon uses. It does
not require a microphone, a daemon, or a Wayland session:

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
nvstt model install         # explicitly download and install Parakeet
nvstt model status [--json] # local model file check
nvstt model path             # expected model directory
```

See [IMPLEMENTATION_SPEC.md](./IMPLEMENTATION_SPEC.md) for the accepted
architecture and milestone plan.
