# Programmatic text input on Wayland

## Executive result

Wayland has no core API that lets an unrelated client type into the client that
currently has keyboard focus. This is deliberate. A Wayland client receives
input for its own surfaces, while the compositor controls routing between
clients. A voice-to-text daemon therefore needs a compositor-approved input
path, or a Linux virtual input device.

Use a `TextSink` abstraction. Detect the available path at runtime and keep the
recorder/transcriber independent from it. The preferred order is:

1. `zwp_virtual_keyboard_v1` for compositors that expose and authorize it.
2. XDG RemoteDesktop portal, then `libei`/EIS, with keyboard consent.
3. A documented compositor-specific adapter, when needed.
4. An explicit user opt-in to `uinput` (`ydotool` or `dotool`).
5. Clipboard copy as a last resort, with a prompt for the user to paste.

Do not select a backend from the compositor name alone. Bind the Wayland
registry and the portal interfaces, check protocol versions and capabilities,
and report the exact failure to the CLI. The same compositor family can ship
different protocol sets or security policy.

## Why the core protocol cannot type into another client

The Wayland protocol is a client/compositor protocol. The core input objects
deliver keyboard events to the focused surface chosen by the compositor. Core
data-device objects provide selection and clipboard transfer, but they do not
insert text into another client. There is no core “send key” or “send text”
request.

This isolation is a security property. A background client cannot silently
drive another application merely because both applications use Wayland. Any
solution that works across clients must be exposed and controlled by the
compositor or by the Linux input layer.

Reference: [Wayland protocol overview](https://wayland.freedesktop.org/docs/book/Protocol.html)
and the [core protocol XML](https://chromium.googlesource.com/external/wayland/wayland/+/refs/heads/master/protocol/wayland.xml).

## Protocols that can provide text input

### `zwp_virtual_keyboard_v1`

This unstable protocol is implemented by a number of wlroots and Smithay
compositors. A client obtains a virtual keyboard for a `wl_seat`, supplies an
XKB keymap through a file descriptor, and sends key press/release events and
modifier state. The protocol description explicitly says that it emulates a
physical keyboard. The manager may reject the client as `unauthorized`.

The protocol is suitable for a daemon that must work in arbitrary focused
widgets. It sends normal keyboard events through the compositor, so the target
application does not need to implement a text-input protocol. It is still an
unstable protocol, and its presence does not imply that the compositor will
authorize every client.

References:

- [Virtual keyboard protocol XML](https://git.nixnet.services/blankie/wlroots/src/commit/1491ec42daf942350727fe6e158ac6ead669f643/protocol/virtual-keyboard-unstable-v1.xml)
- [wlroots virtual keyboard API](https://agx.pages.freedesktop.org/wlroots/wlr/types/wlr_virtual_keyboard_v1.h.html)
- [wtype](https://man.archlinux.org/man/wtype.1.en), a small tool that uses this protocol
- [Sway issue showing advertised globals](https://github.com/swaywm/sway/issues/8204)

Use `xkbcommon` to build a temporary keymap and map Unicode text to key
sequences. Keep all modifier and key state balanced. If the compositor returns
`unauthorized` or closes the object, move to the next backend.

### Text-input and input-method protocols

`zwp_text_input_v3` is intended for the focused application and its text
widgets. An application enables it when an editable field has focus. An input
method then sends `preedit_string`, `commit_string`, and deletion requests.
The receiving application inserts the committed text. A standalone voice
daemon is not a text-input object for the target application; it would need to
act as the compositor's input method.

The matching input-method protocol is also experimental. It is one object per
seat, connects to the compositor's focused text input, and has its own
authorization and lifecycle. It is useful for IME composition, but it cannot
guarantee insertion into applications that do not support the text-input
protocol or into widgets that expose no text-input state. It is not a reliable
“type into any focused client” API.

References:

- [Text input v3 protocol XML](https://chromium.googlesource.com/external/anongit.freedesktop.org/git/wayland/wayland-protocols/+/refs/heads/master/unstable/text-input/text-input-unstable-v3.xml)
- [wlroots input-method v2 protocol](https://sources.debian.org/src/wlroots/0.18.2-3/protocol/input-method-unstable-v2.xml)
- [Current protocol staging index](https://wayland.app/protocols/)

Treat input-method support as an optional adapter for a future IME-style mode,
not as the primary sink for the first release.

### libei (Emulated Input) and EIS

`libei` is the modern emulated-input protocol. A compositor runs an EIS
server, and the application connects as an EI client. The compositor can mark
events as emulated and apply fine-grained policy. Accepted events enter the
normal input path and appear to Wayland clients like keyboard input.

The sender API can emit evdev keycodes and, in recent libei versions, UTF-8
text when the EIS device advertises `EI_DEVICE_CAP_TEXT`. Otherwise the client
must use a keymap and keycodes. Starting and stopping emulation is
transactional: release all pressed keys before stopping the device.

References:

- [libei overview](https://whot.pages.freedesktop.org/libei/index.html)
- [libei API index](https://libinput.pages.freedesktop.org/libei/api/index.html)
- [libei sender API](https://libinput.pages.freedesktop.org/libei/api/group__libei-sender.html)

### XDG RemoteDesktop portal

The portal is the normal way for an unprivileged desktop application to request
remote input. The flow is `CreateSession`, `SelectDevices`, and `Start`.
Request only the keyboard device. The portal normally asks the user to allow
remote control. Version 2 provides `ConnectToEIS`, which returns a file
descriptor for a libei sender. Version 1 also has keyboard keycode and keysym
notification calls.

The portal owns policy and consent. Cache a restore token only when the portal
supports it, and expect the user to revoke or reject access. A first-run prompt
is part of the expected UX, not an error in the application.

Reference: [RemoteDesktop portal specification](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html).

GNOME's remote-desktop stack uses libei for input plumbing ([GNOME remote desktop
source](https://github.com/GNOME/gnome-remote-desktop)). KDE and GNOME support
levels depend on desktop and portal versions, so probe the portal instead of
hard-coding a version assumption. Some wlroots portal backends implement
screencast but not RemoteDesktop; the [xdg-desktop-portal-wlr FAQ](https://github-wiki-see.page/m/emersion/xdg-desktop-portal-wlr/wiki/FAQ)
calls this out as a possible limitation.

## Compositor and window-manager matrix

The entries below are capability expectations, not guarantees. Each startup
must perform a registry and portal probe.

| Environment | Virtual keyboard | libei / RemoteDesktop | Practical note |
| --- | --- | --- | --- |
| Sway and some wlroots WMs | Often exposed; Sway 1.9 advertised the global | Depends on the portal backend; do not assume | Try direct virtual keyboard first |
| Hyprland | Current docs expose `input.virtualkeyboard.*` settings | Portal support varies | Hyprland is independent of wlroots; probe globals |
| Niri (Smithay) | Current source registers virtual keyboard and input-method states | Portal support is not guaranteed | Restricted clients can have protocol globals filtered |
| GNOME / Mutter | Usually no wlroots virtual-keyboard global | Preferred path is RemoteDesktop + libei | Expect explicit user consent |
| KDE / KWin | Usually no wlroots virtual-keyboard global | Prefer RemoteDesktop + libei | Test the installed Plasma/portal version |
| Other wlroots, Smithay, Weston, COSMIC, gamescope | Compositor-specific | Compositor and portal-specific | Never infer support from toolkit or family name |

Evidence for compositor-specific behavior:

- [Hyprland virtual-keyboard settings](https://wiki.hypr.land/0.55.0/Configuring/Basics/Variables/)
- [Niri source registering virtual keyboard and input-method state](https://raw.githubusercontent.com/niri-wm/niri/main/src/niri.rs)
- [Sway source and issue tracker](https://github.com/swaywm/sway/issues/8204)

## Kernel `uinput` fallback

Linux `uinput` lets a process create a virtual input device through
`/dev/uinput`. The kernel then routes its events to input consumers. This works
below Wayland and therefore works on X11, Wayland, consoles, and most
compositors. `ydotool` uses this method and requires a daemon plus permission
to open `/dev/uinput`; a udev rule is safer than running the whole application
as root. The graphical environment may need time to discover the new device.

References:

- [Linux uinput documentation](https://www.kernel.org/doc/html/v4.12/input/uinput.html)
- [ydotool README](https://github.com/ReimuNotMoe/ydotool)
- [ydotool man page](https://manpages.ubuntu.com/manpages/resolute/man1/ydotool.1.html)

`uinput` has no compositor focus or consent model. A permitted process can
inject input into any session, including a lock screen. It can also be blocked
by sandboxing or system policy. Make it an explicit opt-in fallback, run a
permission preflight, and report that it has broad system scope. Do not make it
the default when a portal or compositor protocol is available.

## Clipboard fallback

`wl-copy` can place the transcription in the Wayland clipboard. This does not
insert text into the focused widget. The safe fallback is to copy the text,
notify the user, and ask them to press Ctrl+V. A later implementation may pair
this with a selected key-injection backend to synthesize paste, but must still
preserve and restore the previous clipboard where possible.

Reference: [wl-clipboard](https://github.com/bugaevc/wl-clipboard).

Do not use clipboard fallback for password fields without a clear user action,
and do not log clipboard contents.

## Suggested backend contract

Keep the injection layer behind a small interface:

```text
TextSink::probe() -> capabilities, reason
TextSink::begin() -> session
TextSink::send_utf8(text) -> result
TextSink::end() -> result
```

The implementation can have `WlrVirtualKeyboardSink`, `PortalEISSink`,
`DirectEISink`, `UinputSink`, and `ClipboardSink`. `probe` must return a stable
reason such as:

- `unsupported` (protocol is absent)
- `unauthorized` or `permission-denied`
- `portal-cancelled`
- `focus-lost`
- `layout-unrepresentable`
- `timeout`

The CLI can map these reasons to notifications such as “typing unavailable:
portal permission denied”. Keep a long-lived portal/EIS session only if it has
health checks; otherwise open it for each transcription and release all key
state on completion.

At send time, target the compositor's current keyboard focus. A background
daemon should not try to discover or inspect the target application's surface,
because Wayland does not provide a general privacy-safe API for that.

## Release plan and tests

1. Implement registry probing and a dry-run capability command.
2. Implement direct virtual keyboard with an XKB keymap. Test ASCII,
   punctuation, modifiers, multiline text, and non-ASCII text.
3. Implement RemoteDesktop portal `ConnectToEIS`. Test the consent prompt,
   denial, restore token, revoked permission, and `EI_DEVICE_CAP_TEXT` absent.
4. Add uinput only behind an explicit setting. Test missing `/dev/uinput`,
   insufficient permission, device discovery delay, and clean daemon shutdown.
5. Add clipboard-only fallback and a clear notification that no automatic
   insertion occurred.

Run the matrix on current GNOME/Mutter, KDE/KWin, Sway, Hyprland, and Niri.
Record compositor version, portal implementation, protocol globals, selected
backend, and result. This is more reliable than a static compositor allowlist.

## Open product decisions

- Is an initial user consent dialog acceptable, or must the app require a
  pre-authorized configuration?
- Is broad `uinput` access acceptable as a user-selected fallback?
- Must the first release type into password and secure fields?
- Which Unicode targets are required (Latin only, CJK, emoji, all UTF-8)?
- Should a failed automatic insertion still save the transcript to history and
  copy it to the clipboard?

