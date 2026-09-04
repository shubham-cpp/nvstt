# Language and framework selection

Research date: 2026-08-02.

## Decision

Use a native Rust daemon with a small Rust CLI client. Keep the model, audio,
input, notification, and history concerns behind narrow interfaces. Use
`sherpa-onnx`'s Parakeet Unified 0.6B INT8 buffered-streaming backend as the
default recognizer and keep the CPU execution provider as the baseline. Probe
CUDA as an optional provider on the host.

Use `cpal` for the first audio capture path, with the PipeWire/ALSA-compatible
default source. Add a direct PipeWire adapter only if device selection or
latency tests show that `cpal` is not sufficient. Use `ashpd`/`zbus` for
XDG Desktop Portal calls, a small C FFI or helper boundary for libei/EIS while
Rust bindings mature, and `wayland-client` plus generated protocol bindings for
direct virtual-keyboard probing. Keep `wtype`, clipboard, and `ydotool` behind
the same `TextSink` trait so the first release can ship incrementally.

Use `tokio` for the daemon and Unix-socket IPC, `clap` for the CLI, `serde` +
`toml` for configuration, `tracing` for logs, `notify-rust` for passive desktop
notifications, and a small JSON/SQLite history store selected after the
persistence decision. These crates provide a single native executable and fit
Fedora/Bazzite packaging better than a Python runtime bundle.

## Why Rust is the best first release

| Concern | Rust | Python | Go/C++ |
| --- | --- | --- | --- |
| Parakeet runtime | `sherpa-onnx` exposes Rust bindings and `OnlineRecognizer` streaming support | NeMo is the reference runtime, but PyTorch packaging is heavy | C/C++ APIs exist; cgo/build integration adds work |
| Wayland/portal/IPC | Native Unix sockets, D-Bus, Wayland, and async libraries; C FFI is explicit | D-Bus is usable, but libei and Wayland bindings are less uniform | D-Bus is good; PipeWire/libei ecosystem is smaller or cgo-heavy |
| Audio | `cpal` covers Linux capture and can use the desktop's compatibility layer | `sounddevice`/PortAudio adds another runtime dependency | Native APIs are possible but require more platform code |
| Distribution | One binary plus a separately managed model archive | Python version, wheels, virtual environment, and native libraries must align | Good binary story, but fewer ready-made ASR bindings |
| Reliability | No GIL; bounded state and cancellation are straightforward | A long-lived daemon is viable but packaging and native callbacks are harder | Strong, but fewer reusable crates/APIs for this exact stack |

Python remains useful for model parity tests and research notebooks. A separate
Python worker is not justified for the first product: it adds IPC, duplicate
state, and a second failure/packaging surface when sherpa-onnx already has a
Rust path.

## Proposed first-release boundaries

```text
CLI -> Unix socket -> daemon state machine
                         |-> Recorder (cpal/PipeWire-compatible source)
                         |-> Recognizer (sherpa-onnx Parakeet Unified INT8 streaming)
                         |-> TextSink (portal/libei, virtual keyboard, clipboard, uinput)
                         |-> History (bounded last-10 policy)
                         `-> Notifier (desktop notification + JSON status)
```

The daemon should own one warm recognizer and one long-lived input session when
the portal permits it. `toggle` is a local IPC command, not a second recorder
process. The output layer must report transcription success separately from
text-delivery success.

## What to defer

- direct streaming/partial text and automatic VAD; manual start/stop is easier
  to test and matches the requested toggle contract;
- a custom system-wide IME using `text-input`/`input-method`; it conflicts with
  configured desktop IMEs and does not cover every client;
- a graphical overlay or tray UI; notifications and `status --json` avoid focus
  theft while GNOME/KDE/wlroots behavior is tested;
- a mandatory CUDA or PyTorch distribution; CPU must work on immutable Fedora or
  Bazzite images without a CUDA toolkit;
- LLM rewriting, per-application prompts, cloud providers, and audio retention.

## Sources

- [sherpa-onnx overview and APIs](https://k2-fsa.github.io/sherpa/onnx/index.html)
- [sherpa-onnx Rust crate](https://docs.rs/sherpa-onnx/latest/sherpa_onnx/)
- [NVIDIA NeMo ASR inference](https://docs.nvidia.com/nemo/speech/nightly/asr/inference.html)
- [XDG RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [libei and liboeffis APIs](https://libinput.pages.freedesktop.org/libei/)
- [ashpd Rust portal bindings](https://docs.rs/ashpd/latest/ashpd/)
- [wayland-client Rust crate](https://docs.rs/wayland-client/latest/wayland_client/)
- [cpal audio I/O](https://github.com/RustAudio/cpal)
- [PipeWire Rust bindings](https://pipewire.pages.freedesktop.org/pipewire-rs/)
- [Tokio](https://tokio.rs/), [Clap](https://docs.rs/clap/latest/clap/),
  [notify-rust](https://docs.rs/notify-rust/latest/notify_rust/)
