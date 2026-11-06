# System tray and runtime microphone selection

Research date: 2026-10-03. Local source revision: `68fa528`, with existing working-tree changes.

## Recommendation

The proposed tray is feasible. Use a StatusNotifierItem through `ksni` for this Linux daemon. Keep the daemon as the owner of recording, transcription, and delivery. The desktop panel draws the icon and menu. A separate application window is not required.

For microphone selection, first evaluate CPAL 0.18.2 with its `pulseaudio` feature. This uses the existing PipeWire PulseAudio service on this machine. A small enumeration probe compiled and ran inside the `Fedora` distrobox. It found both physical microphone sources and the Easy Effects source. This is stronger evidence than assuming ALSA device enumeration matches the desktop input list.

Recommend switching the selected input between dictations, without restarting nvstt. Changing the input during an active dictation needs a separate design and capture test. Enumeration success does not establish capture reliability.

The library comparison, platform support, and tray APIs are in [the companion library report](16-system-tray-library-options.md).

## Desktop portability requirement

The target is a generic Linux StatusNotifierItem and D-Bus menu. KDE is the inspected desktop, not a required dependency. The same item should work with Plasma, Waybar, and Quickshell-based panels that expose a tray and its menus. Quickshell documents both icon and menu support. See its [tray service](https://quickshell.org/docs/v0.3.1/types/Quickshell.Services.SystemTray/SystemTray/) and [tray item API](https://quickshell.org/docs/v0.3.1/types/Quickshell.Services.SystemTray/SystemTrayItem/).

Use runtime tray-service discovery. Do not gate the tray on `XDG_CURRENT_DESKTOP=KDE`, invoke KDE configuration tools, or require a Plasma widget. The `org.kde` names are shared protocol names. Quickshell's [host implementation](https://raw.githubusercontent.com/quickshell-mirror/quickshell/master/src/services/status_notifier/host.cpp) uses them too. Microphone selection goes through the audio backend, independent of the desktop shell.

The panel controls layout, styling, click bindings, and menu rendering. Quickshell configurations must display the tray items and wire a menu action. GNOME needs a supporting extension. If no compatible host runs, retain hotkeys, CLI control, and notification fallback. Protocol support does not guarantee identical rendering across hosts.

Validate the same nvstt item on Plasma, Waybar, and a Quickshell panel before claiming those targets are tested. Check all five icons, microphone submenu selection, tooltips, and panel restart. No separate application implementation should be needed for each host.

## What the checkout already provides

| Existing code | Finding | Required change |
| --- | --- | --- |
| [`domain.rs`](../../src/domain.rs), `DaemonState` | Starting, idle, listening, finalizing, delivering, and error states already exist. | Publish a small tray snapshot at transitions. |
| [`notifier.rs`](../../src/notifier.rs), `NotificationEvent` | Listening and finalizing events exist. Delivery has outcome events, but no delivery-start event. | Add a delivery-start update. Do not infer typing from `Transcribed`, which precedes history writes. |
| [`app.rs`](../../src/app.rs), `deliver_transcript` | Sets delivering before calling the text sink. It resets to idle after success, clipboard fallback, or failure. | Keep the last delivery outcome separately from the current activity. |
| [`app.rs`](../../src/app.rs), `transcription_failure` | Resets to idle, then emits a failure event. | Retain a visible error until acknowledgment or the next successful start. |
| [`recorder.rs`](../../src/recorder.rs), `CpalRecorder::start` | Opens `cpal::default_host().default_input_device()` for each new dictation. | Resolve a selected device before stream creation. |
| [`config.rs`](../../src/config.rs), `Config::load` | Reads denoise under `[audio]`. There is no input selector or configuration writer in this path. | Add selection parsing and a deliberate persistence policy. |
| [`ipc.rs`](../../src/ipc.rs), `IpcRequest` | Supports toggle, cancel, status, and history. It has no device list, selection, or subscription request. | Add device list and selection commands. A separate tray process would also need state events. |
| [`app.rs`](../../src/app.rs), `run_daemon` | Handles requests serially. Stop joins the recognition worker and performs delivery before replying. | Keep tray D-Bus work independent of these blocking operations. |
| [`delivery.rs`](../../src/delivery.rs), `connect_portal_with_timeout` | Portal setup can block the command path for up to ten seconds per attempt. | Publish delivery activity before waiting. Keep the tray responsive. |

I indexed the checkout and checked coverage for these source files. The graph recorded no coverage gaps and matching metadata. I also read the relevant transition and command code. This is task-directed verification, not an audit of all paths.

## Tray behavior

Five main icons cover the requested behavior. Startup can reuse the idle shape with a starting tooltip and disabled controls.

| Icon | Meaning | Tooltip example |
| --- | --- | --- |
| Ready | No active dictation, no retained failure | `Ready · Digital microphone` |
| Listening | The capture stream has started | `Listening · Digital microphone` |
| Transcribing | Capture has stopped; final recognition and preparation continue | `Finalizing dictation` |
| Typing | Automatic delivery is being attempted, including portal setup | `Sending text · waiting for keyboard permission` |
| Error | Capture, recognition, or delivery failed | A short failure reason, with the stage named |

Inference already runs while listening. The transcribing icon therefore means finalization after stop. The typing icon must not claim that every character has reached the target application.

Keep clipboard fallback distinct in the outcome text. `Copied; automatic typing unavailable` is accurate. It is not automatic typing success. A warning overlay or sixth icon is optional. A canceled or no-speech session can return to ready with an explanatory tooltip.

Suggested menu:

```text
nvstt: Ready
Microphone >
    Follow system default
    Stereo microphone
    Digital microphone
    Easy Effects source
    Refresh inputs
Cancel dictation             [enabled only while listening]
Last result: ...             [disabled text]
Clear last error             [enabled when needed]
```

Use radio selection for the microphones. The selected preference and the input used by an active session can differ. Show both when they differ. Put playback monitor sources in a clearly labeled secondary group, or omit them from the normal microphone list. Keep virtual microphone sources such as Easy Effects available.

The host controls menu placement and mouse gestures. Test right-click and submenus on Plasma, Waybar, and Quickshell. Some hosts also open menus on left-click. Do not require one gesture for a core function. The companion report documents these host limits.

Keep the existing hotkey as the normal stop-and-type action. A tray menu can change keyboard focus. A menu action that types into the current focus needs a separate focus-restoration design. Cancel and microphone selection do not have that delivery problem. Avoid automatic delivery retries, since a failed attempt may already have inserted part of the text.

## Rust mechanisms and lean choices

Rust's standard library has no system tray API. On Linux, the native desktop mechanism is a D-Bus StatusNotifierItem with a D-Bus menu. A Rust crate can implement that mechanism without a GTK window. See [ksni](https://docs.rs/ksni/0.3.6/ksni/) and the [library report](16-system-tray-library-options.md).

`ksni` fits an existing Linux Tokio daemon. It provides icon names or embedded pixel data, tooltips, submenus, radio groups, enabled states, and callbacks. Prefer embedded small icon pixmaps for this deployment. Theme names depend on installed host icons. Container file paths may not resolve in the host panel. Icon changes report activity; the panel decides when to repaint. Do not promise that a very short state remains visible.

Current `tray-icon` also offers a ksni backend. It is a valid alternative. Its broader abstraction is more useful if cross-platform GUI support becomes a requirement. GTK/AppIndicator bindings and direct `zbus` implementations are also possible. For this daemon, direct `ksni` avoids a GUI event loop and manual protocol maintenance. Exact features and dependencies appear in the companion report.

No tray option should perform audio capture or inference inside a menu callback. Send a command to the daemon and return. Update the tray from a separate state snapshot. Tray loss must not stop an active dictation.

The initial dependency proposal is small:

```toml
# Proposed additions and changes, not applied by this research.
ksni = "0.3.6"
cpal = { version = "0.18.2", features = ["pulseaudio"] }
```

Use ksni's Tokio defaults initially. Add Tokio's `sync` feature explicitly if the integration uses Tokio channels. The current application manifest does not request it. Check feature unification with existing D-Bus dependencies before committing the new lockfile. The tray library report records the executor compatibility issue.

## Microphone mechanisms

CPAL already provides `input_devices()`, `DeviceTrait::id()`, `description()`, and `device_by_id()`. Device IDs identify devices within a host. Persist the full host-qualified ID, not a menu position or display name. Re-resolve it before capture and handle disappearance. IDs are stable where the backend can provide stability. They are not a guarantee across profile or device changes. See the [CPAL host API](https://docs.rs/cpal/0.18.2/cpal/traits/trait.HostTrait.html) and [device API](https://docs.rs/cpal/0.18.2/cpal/traits/trait.DeviceTrait.html).

| Approach | Benefit | Limit | Assessment |
| --- | --- | --- | --- |
| Existing CPAL ALSA backend | No new audio dependencies | ALSA aliases and hardware PCMs do not match the desktop source list. Direct hardware can contend with the audio server. | Retain as an explicit fallback, not the first microphone menu. |
| CPAL `pulseaudio` feature | Keeps the recorder API. Exposes named desktop sources through PipeWire PulseAudio. | Adds Rust dependencies and changes the chosen default backend. Capture behavior needs validation. | Best first experiment for this machine. |
| CPAL `pipewire` feature | Native PipeWire nodes and targeted streams | Needs native PipeWire build dependencies. This container currently lacks `libpipewire-0.3.pc`. | Good alternative if native graph control becomes necessary. |
| Direct `pulseaudio`, `libpulse-binding`, or PipeWire API | More detailed source metadata, subscriptions, and stream routing | Adds audio-specific lifecycle code. | Use only if CPAL cannot meet the required input behavior. |
| `pactl` or `wpctl` subprocess | Fast manual experiment | External tools, command parsing, and global/default versus per-stream routing distinctions | Useful for diagnosis. Avoid as the initial product dependency. |

The [CPAL 0.18.2 manifest](https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/Cargo.toml) enables both audio-server backends through optional features. The Linux [host selection code](https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/src/platform/mod.rs) prefers PipeWire, then PulseAudio, then ALSA when those features are present and initialization succeeds. Enabling an audio feature therefore changes capture behavior, even if `default_host()` remains unchanged in nvstt.

The PulseAudio backend uses the Rust [`pulseaudio` protocol crate](https://docs.rs/pulseaudio/0.3.1/pulseaudio/). It does not add a `libpulse-binding` dependency. The probe's direct CPAL dependencies were `alsa`, `dasp_sample`, `futures`, `libc`, and `pulseaudio`. Existing ALSA native linkage remains. Do not describe the entire nvstt binary as pure Rust.

PulseAudio [enumeration and device IDs](https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/src/host/pulseaudio/mod.rs) use server source names. PipeWire [device code](https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/src/host/pipewire/device.rs) uses node names for saved IDs and current object serials for stream targeting. Its host stores a device snapshot. Recreate the host when refreshing devices rather than expecting an old snapshot to discover hotplugged inputs. PipeWire also exposes sink capture as input-capable devices, so `input_devices()` alone is not a microphone filter.

`Follow system default` should resolve the default at the start of each session. Do not assume identical live rerouting across backends. CPAL documents backend-dependent default-change behavior. The current recorder treats backend callbacks as capture faults. Any new handling of a device-change callback must preserve the existing audio integrity contract.

Changing nvstt's input should affect nvstt only. Do not call `wpctl set-default` for that action. It changes the system default for new automatically connected streams. See [WirePlumber's command documentation](https://pipewire.pages.freedesktop.org/wireplumber/man/wpctl.html#set-default).

## What runtime switching should mean

For the first release, accept selection while idle and apply it to the next dictation. Disable selection while busy and enforce that rule in the daemon. Menu state alone is insufficient. A selection request can race with a hotkey. An invalid or missing device must return a clear error without changing the previous selection.

Use an explicit policy when a saved microphone disappears. Recommend keeping that preference visible as unavailable and asking the user to select another input. `Follow system default` remains an available choice. Silent substitution can capture a different microphone than the user expects.

Runtime-only selection is the smallest implementation. Remembering it after restart needs a configuration write. Preserve unrelated TOML content or store the preference in a separate small settings file. The current read-only loader does not solve safe writes.

Mid-dictation switching is technically possible, but it is a different feature. Reopening a stream introduces a gap and may change its rate, channels, and callback timing. The current audio source fixes its sample rate for one worker and one saved recording. A switch would need defined queue draining, new conversion state, recording metadata, and failure rules. Server-side stream movement may preserve the client format, but it still needs capture-gap tests. Do not promise seamless switching based on device enumeration.

## Integration architecture

Prefer one nvstt daemon with an optional tray module:

```text
Hotkey / CLI ─┐
              ├─> command queue ─> serialized daemon controller ─> recorder / delivery
Tray menu ────┘                              │
                                            └─> activity and outcome snapshot
                                                        │
                                             independent tray service
                                                        │
                                                host desktop panel
```

The controller remains the single owner of session transitions. Publish activity before slow work and publish its outcome afterward. Keep D-Bus updates and menu callbacks on an independent runtime task. The existing synchronous request path must not hold a tray lock during worker joins or portal waits. A dedicated controller thread is one practical implementation. It requires deliberate shutdown handling, not a detached worker per click.

Keep the tray protocol status `Active` during idle. Some hosts hide `Passive` items, which would also hide the microphone menu. Use the icon and tooltip to express application activity. The companion report explains the protocol's separate `NeedsAttention` state.

A small tray snapshot should include activity, last outcome, selected input, active input, and availability. Exclude transcript text. Do not make the tray poll `status`: the serial request handler cannot answer during finalization or delivery, and polling can miss brief states. Retain outcome state separately so the error icon survives the transition back to idle.

A separate `nvstt tray` process is also feasible. It would isolate desktop failures and could run outside distrobox while the daemon remains inside. It needs a state subscription, reconnect behavior, device commands, and a shared socket path. That is more work than the in-process tray for the current scope. It may become useful if session bus separation remains difficult.

## Distrobox and desktop observations

These observations came from read-only checks and isolated compilation. I did not change the running daemon, audio routing, notifications, or application dependencies.

| Check | Observed result |
| --- | --- |
| Host desktop | KDE, Wayland |
| Host tray watcher | `org.kde.StatusNotifierWatcher` exists. `IsStatusNotifierHostRegistered` returned `true`. |
| Fedora default session bus | An independent user bus exists at `/run/user/1000/bus`. It has no KDE tray watcher. |
| Fedora access to host bus | Explicit access at `/run/host/run/user/1000/bus` returned `true` for the host tray property. |
| Fedora audio sockets | PipeWire and PulseAudio sockets link into `/run/host/run/user/1000/`. |
| Existing CPAL features in isolated probe | Available host: ALSA. Inputs included duplicate aliases, raw hardware, and `null`. |
| CPAL with `pulseaudio` in isolated probe | Available hosts: PulseAudio and ALSA. Default host: PulseAudio. Both physical microphone sources and Easy Effects were listed. |
| Native PipeWire build probe | `pkg-config` could not find `libpipewire-0.3`. No packages were installed. |

Build location and runtime location are separate choices. Continue building through `distrobox enter --name Fedora`. A host process can use the host session bus directly if its runtime libraries are available. A process inside this init-enabled container must select the host bus deliberately. The tray and keyboard portal must reach the intended desktop session. Do not apply container-specific paths to a host service.

The existing [user unit](../../contrib/nvstt.service) starts `%h/.cargo/bin/nvstt daemon`. Check the actual deployment before changing it. Test graphical logout/login, host watcher restart, and daemon restart. Keep a single daemon instance and retain CLI access when a tray host is absent.

Probe reproduction, outside the repository application manifest:

```bash
rtk proxy distrobox enter --name Fedora -- cargo run \
  --manifest-path /home/shubham/.cache/tray-feasibility-u07037nc/Cargo.toml --quiet
rtk proxy distrobox enter --name Fedora -- cargo run \
  --manifest-path /home/shubham/.cache/tray-feasibility-u07037nc/Cargo.toml \
  --features pulse --quiet
rtk proxy distrobox enter --name Fedora -- busctl \
  --address=unix:path=/run/host/run/user/1000/bus get-property \
  org.kde.StatusNotifierWatcher /StatusNotifierWatcher \
  org.kde.StatusNotifierWatcher IsStatusNotifierHostRegistered
```

The scratch program calls only host and device enumeration APIs. It does not build or play a capture stream. Its manifest pins `cpal = "=0.18.2"`; feature `pulse` enables `cpal/pulseaudio`. The scratch files can be removed with `trash` after review.

## Work needed before implementation is accepted

1. Verify static and changing icons, tooltip updates, right-click, and radio submenus on Plasma, Waybar, and Quickshell.
2. Verify watcher absence and restart without interrupting dictation. Check daemon and tray shutdown together.
3. Add explicit delivery-start and retained-outcome updates. Check clipboard fallback, no speech, cancellation, and failure recovery.
4. Add device list and selection commands. Validate IDs at capture start and reject busy or stale requests.
5. Test each physical and virtual input with actual capture. Check unplug, reconnect, suspend, backend loss, sample-rate changes, and stop-tail integrity.
6. Check notification policy. Recommend suppressing routine phase notifications when the tray is visible. Keep actionable failure feedback and a fallback when no tray host exists.
7. If selection persists, check restart behavior and safe writes that preserve other settings.

No live capture, tray rendering, typing, hotplug, latency, or memory measurements were performed. The isolated probes establish desktop-bus access and device discovery only. This report proposes a design; it does not implement or approve the later feature work.
