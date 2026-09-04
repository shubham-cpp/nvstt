# KDE Plasma / KWin Wayland input research

This report covers how a long-running voice-to-text daemon can insert text into
the focused application on KDE Plasma with the Wayland session.

## Short answer

Use the XDG RemoteDesktop portal and its libei/EIS connection as the primary
input path. Ask for keyboard control once, keep the session alive, and cache the
portal restore token when the portal supports it. Use the keyboard event path for
short text that the current keyboard layout can represent. For arbitrary UTF-8
text, place the transcript in the clipboard and inject `Ctrl+V`.

Do not make `zwp_virtual_keyboard_v1` (the protocol used by `wtype`) the KDE
baseline. KWin releases reported in KDE bug 512996 did not implement it. A
generated protocol support table now lists newer KWin versions, so the daemon
must probe the compositor at runtime.

Do not use `org_kde_kwin_fake_input`. KDE documents it as a private, privileged
testing/desktop-integration interface. It is not a stable application API.

KWin's input-method protocol is useful only when the application is the selected
KWin input method (an IME). It is not a general text-insertion endpoint for a
separate voice daemon.

## Candidate mechanisms

### 1. `zwp_virtual_keyboard_v1` (the `wtype` route)

The protocol creates a virtual keyboard for a Wayland seat. A client uploads an
XKB keymap, then sends key and modifier events. The protocol description says it
can emulate a physical keyboard or accompany an input method:

* [Wayland virtual-keyboard-unstable-v1 specification](https://wayland.app/protocols/virtual-keyboard-unstable-v1)
* [`wtype` manual](https://man.archlinux.org/man/wtype.1.en)

This is attractive because it needs no X11 server. It is not dependable on KDE,
however. [KDE bug 512996](https://bugs.kde.org/show_bug.cgi?id=512996) records
that KWin 6.3.6 and 6.5.3 did not implement
`zwp_virtual_keyboard_manager_v1`; the report notes that wvkbd, squeekboard and
Maliit then fail to create a virtual keyboard. The `wayland.app` page contains a
generated support table that may list KWin 6.6. This mismatch is a reason to
probe the active compositor with `wayland-info` and to retain another path.

The protocol also permits a compositor to reject an untrusted client. Treat a
successful global-name probe as necessary but not sufficient: handle a creation
error and an authorization failure.

### 2. KWin input-method-v1 (Plasma Keyboard)

The [input-method-unstable-v1 protocol](https://wayland.app/protocols/input-method-unstable-v1)
is an IME protocol. The input method receives a context for the active text
input and can call `commit_string` to insert text. This gives a real UTF-8 text
channel, but only for an active text-input client and only for the one input
method that KWin selected for the seat.

KDE's [Plasma Keyboard project](https://github.com/KDE/plasma-keyboard) confirms
the intended integration: it wraps Qt Virtual Keyboard and uses
`input-method-v1` to communicate with the compositor. Its README also shows that
the IME is configured in `kwinrc` with `Wayland/InputMethod`. A voice daemon could
become that configured IME, but then it would replace or compete with Fcitx,
Maliit, or Plasma Keyboard. It would also need to implement the IME lifecycle,
focus, preedit, deletion, and serial rules. That is a different product from a
CLI-controlled daemon and is not a good first implementation.

KWin exposes D-Bus controls for the configured virtual keyboard:

* [KWin `virtualkeyboard_dbus.cpp` (Plasma 6.3.6 source)](https://sources.debian.org/src/kwin/4%3A6.3.6-1/src/virtualkeyboard_dbus.cpp)

The interface has `setEnabled`, `setActive` and `forceActivate`, plus state
properties such as `isAvailable` and `activeClientSupportsTextInput`. It controls
visibility and activation of the configured IME; it does **not** provide a
general `insertText` method for an unrelated client.

### 3. `org_kde_kwin_fake_input`

KWin's fake-input protocol exposes pointer, touch and keyboard events after an
authentication request:

* [KDE fake-input protocol](https://wayland.app/protocols/kde-fake-input)

The protocol text warns that clients must not use it as a normal application API,
that the compositor can reject events, and that it is an implementation detail
for trusted desktop components. It is intended for cases such as KDE Connect and
testing. A voice daemon must not depend on it.

### 4. XDG RemoteDesktop portal + libei/EIS (recommended)

`libei` is the freedesktop protocol for emulated input. `libeis` is the
compositor-side server, and `liboeffis` is the helper that starts a portal
session and connects to EIS:

* [libei overview](https://libinput.pages.freedesktop.org/libei/)
* [libei API](https://libinput.pages.freedesktop.org/libei/api/index.html)
* [liboeffis API](https://libinput.pages.freedesktop.org/libei/api/group__liboeffis.html)
* [libei README (Debian source mirror)](https://sources.debian.org/src/libei/1.3.901-1/README.md)

The [XDG RemoteDesktop portal specification](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
defines this flow:

1. Call `CreateSession`.
2. Call `SelectDevices` with the `KEYBOARD` bit (`1`). Do not request pointer,
   touch, or screen capture devices.
3. Call `Start`. The portal normally presents a user-consent dialog. With
   `persist_mode=2` (until revoked), a restore token can avoid a prompt on later
   starts. Store the token with user-only permissions and handle token expiry.
4. Call `ConnectToEIS` and pass the returned file descriptor to libei. Keep this
   context and session alive for the daemon lifetime.

After `Start`, the portal's `NotifyKeyboardKeycode` and
`NotifyKeyboardKeysym` methods are available. With `ConnectToEIS`, keyboard
events go through libei instead. The libei README explains why this is a good
fit for a daemon: emulated input is negotiated and authenticated, and the
protocol is not designed for a short-lived fire-and-forget process. It also
states that the compositor can identify, filter, pause, or discard events (for
example, on a password prompt or a locked screen).

The input stream is still keyboard input, not a universal UTF-8 text pipe. The
compositor supplies the expected keymap and the client sends keycodes and
modifiers that match it. Layout-dependent direct typing is suitable for simple
ASCII. It is not a reliable way to type every Unicode character, emoji, or CJK
text.

KDE's portal implementation has shipped keyboard notification support. See
[KDE bug 456025](https://bugs.kde.org/show_bug.cgi?id=456025), which records the
fix for `NotifyKeyboardKeysym`. Verify the active Plasma build at startup; portal
versions and policy settings vary by distribution.

### 5. Clipboard followed by paste

For a transcript with arbitrary UTF-8, the practical method is:

1. Set the Wayland clipboard to the transcript.
2. Use libei to press and release `Ctrl+V` in the focused client.
3. Optionally restore the old clipboard after a short delay.

Wayland's standard data-device protocol carries clipboard data through MIME
offers and file descriptors. See the [Wayland protocol book](https://wayland.freedesktop.org/docs/book/Protocol.html).
The newer [`ext-data-control-v1`](https://wayland.app/protocols/ext-data-control-v1)
protocol gives privileged clipboard managers explicit control of selections; it
was added to wayland-protocols 1.39 ([announcement](https://lists.freedesktop.org/archives/wayland-devel/2024-December/043920.html)).

Clipboard access is compositor/version dependent. A background client using the
ordinary `wl_data_device` path may not have the focused-surface serial needed to
claim the selection. `ext-data-control-v1` may be available on newer KWin builds,
but the daemon must test for it. KWin bug reports also show regressions around
`wl-copy` and clipboard ownership:

* [KDE bug 511840](https://bugs.kde.org/show_bug.cgi?id=511840)
* [KDE bug 520323](https://bugs.kde.org/show_bug.cgi?id=520323)

Treat clipboard+paste as a fallback with clear status reporting. It may fail in
password fields or applications that disable paste. Test both native Wayland and
XWayland clients.

### 6. Linux `uinput` (`ydotool` / `dotool`) fallback

`uinput` creates a kernel virtual input device. It works below the compositor, so
KWin, XWayland, terminals, and TTY applications see normal hardware-like key
events:

* [`ydotool` project](https://github.com/ReimuNotMoe/ydotool)

`ydotoold` normally needs access to `/dev/uinput` (often root or an input-group
rule). This adds installation and security cost, and it bypasses compositor
consent. Keyboard layout and Unicode still need a keymap or clipboard strategy.
Offer it as an explicit, opt-in fallback, not as the default KDE path.

## Compatibility matrix

| Mechanism | KWin Wayland scope | Arbitrary Unicode | Consent / privilege | Recommendation |
|---|---|---|---|---|
| `zwp_virtual_keyboard_v1` / `wtype` | Global keyboard when implemented; absent in KWin versions reported by bug 512996 | Keymap-dependent; can upload XKB map | Compositor may reject untrusted clients | Probe only; never the sole KDE path |
| KWin input-method-v1 | Active text fields; one configured IME per seat | Yes, through `commit_string` | Must be selected as KWin IME; conflicts with other IMEs | Defer unless the daemon becomes a full IME |
| `org_kde_kwin_fake_input` | Global, but private/allowlisted | Keysyms available | Explicit authentication; compositor can ignore | Do not use |
| RemoteDesktop portal + libei/EIS | Global focused-seat keyboard after portal grant | Keymap-dependent; use clipboard for full UTF-8 | User prompt; persistent restore token where supported | Primary path |
| Clipboard + libei `Ctrl+V` | Focused text target | Yes, app and clipboard permitting | Clipboard protocol and compositor policy | Primary Unicode fallback |
| `uinput` (`ydotool`) | Global hardware-like input | Keymap-dependent; clipboard for full UTF-8 | `/dev/uinput` permissions; bypasses compositor | Optional explicit fallback |

## Suggested daemon design for KDE

* Keep one long-running portal/libei session. Do not launch `wtype` or a portal
  process for each transcription.
* At startup, detect Wayland and query the portal version and device types. Also
  probe protocol globals with `wayland-info`; this handles KWin point releases
  and vendor patches.
* Request keyboard only. Show a status notification when consent is needed,
  when the session connects, and when it is revoked or disconnected.
* On transcription completion, choose direct key events only when every character
  is representable by the negotiated keymap. Otherwise use clipboard+`Ctrl+V`.
  Always release keys in an error path.
* Cache and renew the portal restore token. Never log the token or transcript.
* Keep a user opt-in `uinput` backend for systems where the portal is disabled.
  Make its extra permission requirement explicit.
* Do not require a global Wayland key listener. Bind the CLI toggle command to
  KDE Global Shortcuts or let the user call it from a shell. Wayland does not
  provide a portable global hotkey API for arbitrary clients.

## KDE test matrix

Test at least Plasma/KWin 6.3, 6.5 and 6.6 (or the versions shipped by the
target distributions), with:

* native GTK, Qt, Electron and browser text fields;
* XWayland applications and a terminal;
* ASCII, accented Latin, CJK, emoji and multiline text;
* password fields, clipboard managers, an occupied clipboard and paste-disabled
  applications;
* portal denial, restore-token expiry, daemon restart, screen lock and session
  unlock;
* no `zwp_virtual_keyboard_v1`, then an implementation that advertises it;
* no `ext-data-control-v1`, then a compositor that advertises it.

