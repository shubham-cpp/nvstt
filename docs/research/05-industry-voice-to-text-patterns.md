# Industry and open-source voice-to-text patterns

Research date: 2026-08-02.

This report studies MacWhisper, Superwhisper, Whispering, and Linux dictation
tools.  It focuses on behavior that `nvstt` can copy.  Product documentation
does not expose every private implementation detail.  The report labels those
details as unknown instead of guessing.

## Executive answer

The established pattern is a small state machine:

```text
idle -> listening -> processing -> output -> success/error
```

The user starts and stops the state with a global shortcut or a command.  The
daemon records one short utterance, runs a local ASR model, and sends text to
the focused application.  Voice activity detection (VAD) is an optional way
to stop after a pause.  It is not required for the first manual-toggle mode.

The products also share these practices:

- Keep the model warm.  First load can take minutes; later loads are faster.
- Keep capture and output separate.  A recording window or overlay must not
  steal focus from the target application.
- Use a text-output ladder.  Prefer a native Wayland text injector, then use a
  clipboard paste, then a virtual-keyboard fallback.
- Make cloud processing opt-in.  Local products keep audio and history on the
  device unless a cloud engine or an external LLM is selected.
- Keep a visible status.  Tray indicators, a small overlay, audio cues, OS
  notifications, and machine-readable status are common.

For the `nvstt` first release, use a long-running daemon and a CLI client over
a local IPC channel.  Keep only the last ten successful transcripts.  Store
the transcript and timestamps; do not retain audio unless a future setting
explicitly enables it.

## Evidence boundary

MacWhisper and Superwhisper are proprietary.  Their support pages describe
observable workflows, model choices, and fallbacks, but not all internal code.
Whispering is an open-source project, but its current implementation is part
of the Epicenter desktop surface.  Linux projects publish more details about
Wayland output and compositor integration.  The recommendations below are
design inferences from those documented behaviors, not claims about private
code.

## Product comparison

| Product | Activation and lifecycle | ASR and processing | Text delivery | History and status |
| --- | --- | --- | --- | --- |
| MacWhisper | Global shortcut, automatic start, or CLI.  The CLI can stream finalized segments for supported local engines. | Local Whisper, WhisperKit, ParakeetKit, and Apple speech; cloud engines are optional.  Optional prompts can clean, translate, or expand text. | Dictation writes into a text field.  Global mode can copy to the clipboard. | Persistent recordings and transcript history.  Notifications and an overlay are available. |
| Superwhisper | Toggle, push-to-talk, or cancel shortcut.  A recording window shows model loading, processing, and completion. | `whisper.cpp` Whisper models; local Parakeet through the Argmax WhisperKit SDK; cloud models; optional mode-specific AI processing. | Paste into the active app.  It can restore the old clipboard or simulate keypresses when direct paste fails. | Recordings and metadata live under `~/superwhisper/recordings`; history supports search and reprocessing. |
| Whispering / Epicenter | Record with manual stop or VAD.  Browser and desktop choose different recorder implementations at build time. | Provider interface supports local and remote engines.  Desktop exposes on-device GGUF transcription; transformations are separate services. | Native delivery when permitted; browser fallback is clipboard. | Yjs state plus external audio blobs; toast/OS notifications report loading, success, and errors. |
| Handy-Wayland | Shortcut starts/stops or push-to-talk.  A running instance accepts CLI flags such as `--toggle-transcription`. | `whisper-rs`, Parakeet via `transcribe-rs`, and Silero VAD. | `wtype`, `dotool`, or `xdotool`; clipboard fallback.  Linux overlay is disabled by default because it can steal focus. | Tauri tray/window and status; Unix signals and DE/WM commands can control it. |
| Voxtype | Hold a compositor hotkey, or use toggle mode.  It is a local-first daemon. | Whisper, Parakeet, Moonshine, SenseVoice, Paraformer, Dolphin, and Omnilingual engines.  Optional post-processing and spoken replacements. | `wtype` first, then `dotool`, `ydotool`, or clipboard.  It documents compositor-specific keybinding limits. | `voxtype status --follow --format json` supports Waybar and other status bars. |
| Vocalinux | Toggle or push-to-talk; evdev global hotkeys; audio feedback and tray state. | whisper.cpp, OpenAI Whisper, or VOSK.  Silero VAD when ONNX Runtime is available, amplitude fallback otherwise. | Wayland paste uses `wl-copy` plus `ydotool`; X11 and GNOME focus races have explicit fixes. | Configurable JSON settings and tray status; failed focus commits are handled by a FocusIn gate. |
| Speech Note | CLI actions start listening, start-listening-clipboard, or start-listening-active-window. | Multiple offline engines, including whisper.cpp, Faster Whisper, Vosk, Coqui, and april-asr. | X11 works directly.  Wayland active-window insertion needs `ydotool` and its daemon. | Tray/hidden mode and Flatpak permissions; global shortcut portal support is limited to newer GNOME/KDE releases. |

## MacWhisper

MacWhisper's [Dictation guide](https://docs.macwhisper.com/article/14-how-to-use-the-dictation-feature)
says that a shortcut records speech and inserts the transcription into the
current text field.  A prompt can run after transcription to clean or format
the result.  Prompts can be selected per application.

Its [Global mode](https://docs.macwhisper.com/article/16-global) shows an
overlay, can start recording automatically, and can copy the result to the
clipboard.  The [CLI](https://docs.macwhisper.com/article/57-macwhisper-command-line-tool)
talks to a running app over a local socket.  It can select local models, write
transcripts to standard output, persist history, and use `--stream` for
finalized segments.  The stream option is best with local WhisperKit,
ParakeetKit, or Apple speech; cloud engines return one final result.

MacWhisper documents local Whisper and Parakeet processing as on-device.  A
cloud provider receives audio, and a prompt provider receives transcript text.
See [privacy and local processing](https://docs.macwhisper.com/article/52-keeping-transcriptions-private).
Its [dictation troubleshooting page](https://docs.macwhisper.com/article/44-dictation-not-working-in-specific-apps)
also matters for Linux: direct text-field integration depends on the target
application following accessibility conventions.  An explicit “allow
dictation everywhere” option is needed for non-standard clients.

Useful implications for `nvstt`:

- A CLI should control one long-lived process.  A socket or equivalent IPC is
  more reliable than starting a new ASR process for each toggle.
- Stream finalized segments only after the batch path works.  It changes the
  output contract and needs cancellation and partial-text rules.
- Treat app-specific context and LLM rewriting as later features.  The first
  release needs only Parakeet and direct text output.

## Superwhisper

Superwhisper describes a three-stage workflow: dictate, transcribe, and smart
process.  Its [introduction](https://superwhisper.com/docs/get-started/introduction)
lists voice, message, email, note, meeting, and custom modes.  Modes provide
context and optional post-processing.  Its [voice model page](https://superwhisper.com/docs/models/voice)
states that local Whisper models use `whisper.cpp`; local Parakeet uses the
Argmax WhisperKit SDK.  Parakeet is fast on long recordings but can produce
single-word hallucinations or weaker punctuation, so text cleanup is a
separate concern.

The [shortcut guide](https://superwhisper.com/docs/get-started/settings-shortcuts)
defines toggle, push-to-talk, and cancel behavior.  The [advanced settings](https://superwhisper.com/docs/get-started/settings-advanced)
describe practical output fallbacks:

- paste the result into the active application;
- restore the clipboard that existed before recording;
- simulate keypresses when direct paste is blocked;
- keep the result in the clipboard even when the recording window is disabled.

The [recording window guide](https://superwhisper.com/docs/get-started/interface-rec-window)
uses distinct loading, processing, and completed states.  The [history guide](https://superwhisper.com/docs/get-started/interface-history)
keeps the original voice input and the processed text, and supports search and
reprocessing.  Recordings and metadata are stored locally under
`~/superwhisper/recordings`; users can clean them manually or with a schedule.

Superwhisper's [context documentation](https://superwhisper.com/docs/common-issues/context)
captures selected text, clipboard contents, and active-application context.
The timing is important: focus and selection are read at recording start or
before AI processing.  This supports a Linux rule for `nvstt`: capture the
target surface before any notification or overlay can change focus.

## Whispering and Epicenter

The old Whispering repository is archived and moved to Epicenter.  The current
[README](https://github.com/EpicenterHQ/epicenter/blob/main/apps/whispering/README.md)
describes one Svelte application that runs in a browser or the Epicenter
desktop surface.  The browser uses Media APIs, IndexedDB, and clipboard
fallbacks.  The desktop surface selects a native recorder, native OS
permissions, on-device GGUF transcription, and native delivery when allowed.
Audio leaves the device only when the selected provider requires upload.

The current [architecture document](https://github.com/EpicenterHQ/epicenter/blob/main/apps/whispering/ARCHITECTURE.md)
uses build-time platform modules (`index.browser.ts` and `index.tauri.ts`) and
runtime dependency injection for the transcription provider.  It separates
recording, transcription, transformation, storage, and notifications.  A
typed `Result` error path and report sinks feed loading, success, and error
toasts.  This is a useful separation for `nvstt`, even if a Linux daemon does
not need a Svelte UI or Yjs.

The archived project's [README](https://github.com/edwardmanhattan/whispering)
also documents a VAD mode: one shortcut starts listening, speech starts the
recording, and a brief pause stops it.  Local recordings and transcripts are
stored in browser data; an external provider is used only when selected.
The exact chunking and model inference algorithm is not specified.  Do not
copy an assumed algorithm from this project.

## Linux projects with direct Wayland lessons

### Handy-Wayland

The [Handy-Wayland README](https://github.com/danielrosehill/Handy-Wayland)
is close to the target use case.  It starts or stops dictation with a shortcut,
supports push-to-talk, and exposes `--toggle-transcription` to a running
instance.  Its Rust/Tauri backend uses `cpal`, `whisper-rs`, Parakeet through
`transcribe-rs`, and Silero VAD.  On Linux it tries `wtype`, `dotool`, and
clipboard or `xdotool` fallbacks.

Handy disables its Linux layer-shell overlay by default.  The project reports
that an overlay can steal focus and cause clipboard text to paste into the
wrong window.  This is a strong warning for any status UI in `nvstt`: prefer
notifications and a status command, or make an overlay passive and optional.

### Voxtype

[Voxtype](https://github.com/peteonrails/voxtype) is a local-first Linux
daemon.  Its documented flow is compositor hotkey or toggle, audio capture,
local ASR, optional post-processing, and text output.  It supports Parakeet
and several other engines, loads and unloads engines dynamically, and offers
spoken punctuation and replacements.

Its [Wayland output documentation](https://github.com/peteonrails/voxtype)
prefers `wtype`, then `dotool`, then `ydotool`, and finally clipboard output.
It notes that `wtype` handles Unicode and CJK well, while `dotool` follows the
current XKB layout.  Its compositor notes are especially relevant: Hyprland,
Sway, and River can bind key press and release; KWin generally cannot provide
key-release behavior, so toggle mode is the safe KDE path.  A status stream in
JSON is designed for Waybar.

### Vocalinux

[Vocalinux](https://github.com/jatinkrmalik/vocalinux) provides more focus and
VAD safeguards.  It uses evdev global hotkeys, supports toggle and push-to-talk,
and uses Silero VAD with an amplitude-threshold fallback.  Its configuration
exposes VAD sensitivity and silence timeout.

Its release notes document Wayland paste through `wl-copy` plus `ydotool`, and
a GNOME Wayland FocusIn gate.  That gate waits for the target input surface
before committing text, because an early commit can be dropped.  This pattern
should be represented as a retry or readiness check in the `nvstt` output
adapter.

### Speech Note

[Speech Note](https://github.com/mkiol/dsnote) shows a useful CLI shape for a
tray application.  It accepts `start-listening`,
`start-listening-clipboard`, and `start-listening-active-window` actions.  It
works offline with several ASR engines.  Its documentation states that
Wayland insertion needs an external `ydotool` daemon and Flatpak socket
permission, while X11 insertion works without that extra service.  Its global
shortcut portal path currently depends on recent GNOME and KDE releases.

### whisrs

[whisrs](https://y0sif.github.io/whisrs/) is a current Rust daemon and CLI
project that lists GNOME, KDE, Hyprland, Sway, Niri, and other targets.  It
describes local whisper.cpp, cloud, and ASR sidecar backends, true streaming,
and layout-aware typing through `uinput` with XKB reverse lookup.  It also
lists compositor-specific adapters such as a GNOME extension and KDE D-Bus.
These are useful ecosystem signals, but the claims are project documentation,
not an independent compatibility test.

## Reusable architecture patterns

### 1. State and command protocol

Use explicit states: `idle`, `listening`, `processing`, and `error` (with a
short-lived `success` notification).  Make `toggle` legal only from `idle` or
`listening`.  Return a stable error for a toggle during `processing`; do not
start a second recognizer or lose the first result.

Use a single long-lived daemon.  A local Unix socket is a good first IPC
choice.  The CLI can expose:

```text
nvstt toggle
nvstt status [--json]
nvstt history [--limit 10]
nvstt cancel
```

This follows MacWhisper's local CLI socket, Handy's single-instance CLI flags,
Voxtype's status stream, and Speech Note's action commands.

### 2. Capture, VAD, and ASR

The baseline is manual start and manual stop.  Record mono PCM at the sample
rate required by the Parakeet runtime, then run one batch transcription.  Add
Silero VAD later as an optional mode.  VAD needs a silence timeout and a
minimum speech duration to avoid empty or one-word recordings.

Do not promise “real-time” text until the backend returns stable partial or
final segments.  MacWhisper documents streaming for selected local engines;
Superwhisper documents realtime words for its Nova cloud model.  These facts
do not prove that every Whisper or Parakeet backend streams.

Keep one recognizer loaded at daemon start.  Publish model-loading status and
avoid blocking the first `toggle` without a clear notification.  Model warm
time and RAM are explicit user trade-offs in Superwhisper's active-duration
setting and MacWhisper's first-load guidance.

### 3. Output injection

Define a `TextSink` interface.  The first implementation should try, in order:

1. compositor- or session-approved Wayland typing (`wtype` or an equivalent
   native path);
2. clipboard write, paste, and clipboard restore;
3. `dotool` or `ydotool` virtual-keyboard fallback.

The exact order can be configured per desktop.  `wtype` gives strong Unicode
behavior, while `dotool` can follow an XKB layout.  `ydotool` needs a uinput
daemon and permissions.  Do not show an overlay over the target while typing.
If a readiness check is needed, wait for the target surface and retry once.

### 4. History and privacy

Use a bounded deque or SQLite table with a hard limit of ten successful
records.  A record needs an ID, UTC timestamp, duration, model, text, and
output result.  Do not add failed or cancelled recordings to this list.

Keep audio retention separate from transcript history.  The default should be
no audio retention.  If audio is added later, make its path and cleanup policy
visible.  This is a smaller and safer default than MacWhisper and Superwhisper,
which retain recordings for search and reprocessing.

### 5. Notifications and machine-readable status

Send a notification for `initialized`, `listening`, `processing`,
`transcribed successfully`, and `transcription failed`.  Also expose a
machine-readable status command.  A status bar integration can poll JSON, as
Voxtype does, without depending on a graphical tray app.

Notifications must not change focus.  A graphical overlay is optional and
should be disabled by default until focus behavior is tested on GNOME, KDE,
Sway, Hyprland, and Niri.

### 6. Configuration and provider boundaries

Keep the first configuration small:

```toml
model = "parakeet-tdt-v3-int8"
```

Still define internal boundaries for `Recorder`, `Recognizer`, `TextSink`,
`History`, and `Notifier`.  The provider interface lets a future Whisper or
cloud backend be added without changing the CLI state machine.  Keep LLM
post-processing out of the first Parakeet path; it adds data-flow, latency,
and privacy choices.

## Recommendation for the `nvstt` MVP

Build a native Linux daemon with a small CLI client.  Load the local Parakeet
model once.  The first `toggle` starts capture.  The second `toggle` stops
capture, transcribes, writes text through a configurable Wayland sink, and
stores one successful history record.  Use a bounded ten-record history and
send non-focus-stealing notifications for every state transition.

Make the output sink and compositor control adapters replaceable.  GNOME and
KDE may need different global-shortcut setup, and wlroots compositors can
provide their own keybinding commands.  The daemon itself should not assume
that a key-release event exists.  This is why a CLI toggle is the correct
common denominator.

Add VAD, streaming partial text, spoken commands, per-application context,
and LLM rewriting only after the manual-toggle path is reliable.  Each feature
changes timing or focus behavior and needs its own tests.

## Open questions for later design

- Should `toggle` during `processing` return an error, or queue a new request?
- Should the daemon keep a model loaded when idle, or unload it after a timeout?
- Which output sink is available on the target session: `wtype`, `dotool`,
  `ydotool`, compositor extension, or clipboard only?
- Does the user want audio files for debugging, or transcript text only?
- Should notifications use desktop portals, a native library, or an optional
  helper command on immutable Fedora/Bazzite systems?

## Sources

- [MacWhisper Dictation](https://docs.macwhisper.com/article/14-how-to-use-the-dictation-feature)
- [MacWhisper Global mode](https://docs.macwhisper.com/article/16-global)
- [MacWhisper CLI](https://docs.macwhisper.com/article/57-macwhisper-command-line-tool)
- [MacWhisper privacy](https://docs.macwhisper.com/article/52-keeping-transcriptions-private)
- [MacWhisper dictation troubleshooting](https://docs.macwhisper.com/article/44-dictation-not-working-in-specific-apps)
- [Superwhisper introduction](https://superwhisper.com/docs/get-started/introduction)
- [Superwhisper voice models](https://superwhisper.com/docs/models/voice)
- [Superwhisper shortcuts](https://superwhisper.com/docs/get-started/settings-shortcuts)
- [Superwhisper advanced settings](https://superwhisper.com/docs/get-started/settings-advanced)
- [Superwhisper recording window](https://superwhisper.com/docs/get-started/interface-rec-window)
- [Superwhisper history](https://superwhisper.com/docs/get-started/interface-history)
- [Superwhisper context](https://superwhisper.com/docs/common-issues/context)
- [Epicenter Whispering README](https://github.com/EpicenterHQ/epicenter/blob/main/apps/whispering/README.md)
- [Epicenter Whispering architecture](https://github.com/EpicenterHQ/epicenter/blob/main/apps/whispering/ARCHITECTURE.md)
- [Archived Whispering README](https://github.com/edwardmanhattan/whispering)
- [Handy-Wayland](https://github.com/danielrosehill/Handy-Wayland)
- [Voxtype](https://github.com/peteonrails/voxtype)
- [Vocalinux](https://github.com/jatinkrmalik/vocalinux)
- [Speech Note](https://github.com/mkiol/dsnote)
- [whisrs](https://y0sif.github.io/whisrs/)
