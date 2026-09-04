# GNOME + Wayland: programmatic text input

Research date: 2026-08-02.

## Recommendation

Use the XDG Remote Desktop portal to obtain a `libei` sender connection. Request
the keyboard device only, keep the session and `libei` context alive in the
voice-to-text daemon, and send the result to the currently focused surface.

Use `libei`'s UTF-8 text capability when the compositor advertises it. The
capability was added in libei 1.6, but compositor support can lag the library.
When it is not available, send keyboard events using the compositor-provided
XKB keymap. Keep a clipboard-plus-Ctrl+V fallback for characters that cannot be
represented by that keymap.

Do not make the unstable `zwp_virtual_keyboard_v1` protocol, AT-SPI keyboard
synthesis, or Mutter's private D-Bus API the GNOME backend. Keep these as
optional, explicitly detected fallbacks only.

## How GNOME input emulation works

`libei` separates an emulated input client (EI) from the compositor's emulated
input server (EIS). The compositor decides which virtual devices exist, which
capabilities they have, and whether events are accepted. To Wayland clients,
accepted events look like normal input events. This design lets a compositor
pause or reject emulated input, for example on a locked screen or password
prompt.

For a normal, non-sandboxed application, the portable negotiation path is:

1. Call `org.freedesktop.portal.RemoteDesktop.CreateSession`.
2. Call `SelectDevices` with the `KEYBOARD` bit (`1`). Include `persist_mode`
   and a saved `restore_token` when the portal supports version 2.
3. Call `Start`. GNOME normally presents a user-consent dialog. Check the
   returned device bitmask; a user may deny keyboard access.
4. For portal version 2, call `ConnectToEIS`. Pass the returned file descriptor
   to `ei_setup_backend_fd()` (or the equivalent binding), create an EI sender,
   wait for the seat/device, and send frames.
5. Close the session when the daemon exits. Reconnect after a compositor or
   portal restart.

`liboeffis` is the small C helper for steps 1-4. It leaves the actual event
transport to `libei`. A Rust application can use a portal binding such as
`ashpd` and an EI binding, or call the same D-Bus API directly.

The Remote Desktop portal's documented `NotifyKeyboardKeycode` and
`NotifyKeyboardKeysym` methods remain a version-1 fallback. They require the
keyboard permission returned by `Start`; sending over the EIS fd is preferred
when `ConnectToEIS` is available because it avoids one D-Bus round trip per
event.

### Text and Unicode

The `ei_text` interface (libei 1.6) adds:

- `ei_device_text_utf8()` for a UTF-8 string; and
- `ei_device_text_keysym()` for a logical XKB keysym independent of the active
  keymap.

The protocol limits one UTF-8 request to 254 bytes (255 bytes including the
terminator), so long transcripts must be chunked. The EIS implementation may
choose not to expose `EI_DEVICE_CAP_TEXT`; probe capabilities at run time.
If text is not exposed, keyboard events use the keymap supplied by EIS. The
client must not assume a US layout, and characters absent from the active map
can fail. Clipboard insertion avoids that layout problem, but still needs a
Ctrl+V (or an equivalent paste command) injected through the authorized input
channel.

## Capability and compatibility matrix

| Mechanism | GNOME/Mutter Wayland | Text/Unicode | Permission and privilege | Recommendation |
|---|---|---|---|---|
| XDG RemoteDesktop + `libei` keyboard | Supported on modern GNOME/Mutter when the portal backend is installed; runtime capability must be checked | Keymap-dependent; works for ordinary text | User consent through portal; no root | **Primary backend** |
| XDG RemoteDesktop + `libei` `ei_text.utf8` | Requires a recent libei and a compositor EIS device with `TEXT` capability; support is still rolling out | Arbitrary UTF-8 in chunks | Same consent; no root | Use when advertised |
| Portal `NotifyKeyboardKeysym/Keycode` | Portal v1 fallback | Keymap/layout dependent; keysym handling varies | User consent; no root | Compatibility fallback |
| `zwp_virtual_keyboard_v1` / `wtype` | Unstable and compositor-specific. GNOME tools and current voice-typing projects route GNOME through libei instead | Can upload a keymap, but behavior depends on compositor authorization | Compositor may return `unauthorized` | Do not use as GNOME default; probe only |
| AT-SPI `generate_keyboard_event` | GNOME accessibility documentation says its device-event-controller path works for X11, not Wayland | Not reliable on Wayland; `keystring` is unused in Mutter's path | Accessibility D-Bus; no root | Do not use for system-wide typing |
| AT-SPI `EditableText.insert_text` / `set_text_contents` | Works only for applications exposing an `AtspiEditableText` object | Direct UTF-8 insertion | Accessibility D-Bus; no root | Optional app-specific enhancement, not a global backend |
| Clipboard (`wl-copy`/Wayland data device) + paste | Clipboard protocol is standard, but setting the clipboard does not itself activate or paste it | Arbitrary MIME/UTF-8 text | No input privilege; paste still needs a key event or user action | Fallback after input authorization |
| `uinput` (`dotool`/`ydotool` style) | Kernel-created device is seen by Mutter and works across Wayland/X11 | Keymap/compose dependent; Unicode may be difficult | Write access to `/dev/uinput` (usually root or a udev/input group); bypasses compositor consent | Opt-in emergency fallback only |
| XTEST/`xdotool` | X11 clients only. XWayland 23.2+ can route XTEST through libei/portal, but native Wayland clients are not reached | X11 keymap dependent | XWayland portal authorization may apply | X11-only fallback |

The `zwp_virtual_keyboard_v1` XML explicitly says that it can emulate a
physical keyboard, but also says an untrusted client should receive an
`unauthorized` error. It is an external, unstable protocol, not part of the
Wayland core protocol.

## Permissions, persistence, and failure modes

- **First run:** `RemoteDesktop.Start` normally opens a GNOME consent dialog.
  Treat a cancelled dialog, a denied request, and a missing portal interface as
  different errors. Tell the user how to retry authorization.
- **Persistent authorization:** portal v2 accepts `persist_mode=2` (until
  explicitly revoked) and returns a single-use `restore_token`. Save the new
  token returned by every successful `Start`; the old token is invalidated when
  consumed. If restore fails, the portal prompts again.
- **Keep the session alive:** libei performs asynchronous seat/device
  negotiation. It is not designed for a connect-send-exit one-shot process.
  A long-running daemon should own one connection and queue text requests.
- **Focus:** the daemon cannot normally query or change global Wayland focus.
  Input goes to whatever surface is focused when the frame is sent. Do not show
  a focus-stealing window while typing. Desktop notifications should be passive.
- **Compositor policy:** EIS can pause or discard events, especially while the
  session is locked or a password prompt is focused. Report a failed or partial
  injection rather than claiming success.
- **Restart:** a GNOME Shell/Mutter, portal, or session-bus restart invalidates
  the EIS fd. Detect disconnect and re-run negotiation. A stale restore token
  must be replaced with the token from the next successful start.
- **Layout and modifiers:** keyboard-capability events use the keymap supplied
  by EIS, not necessarily the daemon's local layout. Avoid manually assuming
  US keycodes. Send a frame with balanced press/release events.
- **Portal availability:** `xdg-desktop-portal` and a GNOME backend
  (`xdg-desktop-portal-gnome`) must be installed and selected for the session.
  A generic GTK backend does not automatically provide RemoteDesktop input.

## AT-SPI assessment

AT-SPI exposes useful semantic operations. An accessible text object can accept
UTF-8 through `insert_text` or `set_text_contents`. This is useful for a
future app-specific adapter when the target clearly exposes
`AtspiEditableText`.

It is not a replacement for global typing. Many browsers, Electron apps,
terminals, games, custom widgets, and password fields do not expose a writable
accessible object. AT-SPI also does not provide a portable “inject into the
currently focused Wayland text field” operation. GNOME's own development notes
state that its device-event-controller synthesis works for X11 but not Wayland;
the Mutter `keystring` argument is not used.

## Packaging signal

The required libraries are packaged by mainstream distributions:

- Fedora packages `libei`, `libeis`, and `liboeffis` (Fedora 44 has libei
  1.5; Rawhide has 1.6).
- Arch Extra ships `libei` 1.6, including `libei.so`, `libeis.so`,
  `liboeffis.so`, headers, and pkg-config files.
- Debian stable (trixie) ships libei 1.3.901; Debian sid ships 1.5. The
  corresponding `liboeffis` development package is available.
- `xdg-desktop-portal-gnome` is shipped by Debian, Arch, and Fedora GNOME
  installations. Version skew matters: check the portal `version` property
  before using `ConnectToEIS`, restore tokens, or `ei_text`.

## Source links

- [XDG RemoteDesktop portal API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html) — session sequence, device bitmask, consent, persistence, key events, and `ConnectToEIS`.
- [libei overview and C libraries](https://libinput.pages.freedesktop.org/libei/api/index.html) — EI/EIS roles, `liboeffis`, pkg-config, and demo clients.
- [liboeffis portal flow](https://libinput.pages.freedesktop.org/libei/libraries/index.html) — `CreateSession` → device selection → `ConnectToEIS` → fd handoff.
- [EI protocol overview](https://libinput.pages.freedesktop.org/libei/) — compositor control, distinguishable emulated events, and access control.
- [EI text interface](https://libinput.pages.freedesktop.org/libei/interfaces/ei_text/index.html) and [sender API](https://libinput.pages.freedesktop.org/libei/api/group__libei-sender.html) — UTF-8/keysyms and capability requirements.
- [Peter Hutterer's July 2026 libei update](https://planet.gnome.org/page2.html) — libei 1.6 text events, portal integration since xdg-desktop-portal 1.17, and persistence since 1.21; compositor support may lag.
- [Virtual keyboard protocol](https://wayland.app/protocols/virtual-keyboard-unstable-v1) — protocol behavior and `unauthorized` error. Wayland Explorer is generated from protocol XML; still runtime-probe support.
- [GNOME AT-SPI API](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/func.generate_keyboard_event.html), [EditableText](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.EditableText.insert_text.html), and [GNOME DEC notes](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/de-controller.html) — semantic text operations versus X11-only event synthesis.
- [Wayland core data sharing](https://wayland.freedesktop.org/docs/book/Protocol.html) — standard clipboard/data-device model.
- [Linux kernel uinput documentation](https://kernel.org/doc/html/latest/input/uinput.html) — virtual device behavior and `/dev/uinput` access.
- [Mutter private API notice](https://mail.gnome.org/archives/commits-list/2018-February/msg09043.html) — `org.gnome.Mutter.RemoteDesktop` is private and has no compatibility promise.
- [GNOME 45 release notes](https://release.gnome.org/45/) — Input Leap Wayland support in the GNOME stack.
- [Distribution packages: Fedora liboeffis](https://packages.fedoraproject.org/pkgs/libei/liboeffis/), [Arch libei](https://archlinux.org/packages/extra/x86_64/libei/), [Debian liboeffis-dev](https://packages.debian.org/stable/libdevel/liboeffis-dev), [Arch GNOME portal](https://archlinux.org/packages/extra/x86_64/xdg-desktop-portal-gnome/), and [Debian GNOME portal](https://packages.debian.org/stable/xdg-desktop-portal-gnome).
- [Voxtype's Wayland output notes](https://voxtype.io/news/) — current ecosystem evidence that GNOME/KDE use an `eitype`/libei path, with wtype as a wlroots-only path and clipboard/uinput fallbacks.

