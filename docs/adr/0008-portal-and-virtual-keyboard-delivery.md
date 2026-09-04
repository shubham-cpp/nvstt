# Native delivery backend order

Status: accepted and implemented

## Decision

Use one `TextSink` that selects native Wayland delivery at send time:

1. XDG RemoteDesktop keyboard permission plus EI/libei, through `eitype`.
2. `zwp_virtual_keyboard_v1`, through `wrtype`, when the compositor advertises
   and accepts that unstable protocol.
3. `wl-copy` clipboard fallback with the native failure reason.

The portal connection is lazy. The daemon does not show a consent dialog at
startup. It waits up to ten seconds for each portal handshake, then falls back
if the portal does not respond. It keeps a successful EI connection alive and
stores a returned restore token in the user state directory with mode `0600`.
A revoked token is retried once without a token so the portal can ask for fresh
consent; a timed-out handshake is not retried in the same delivery attempt.

The direct virtual-keyboard path is runtime-probed. It is useful on Sway,
Hyprland, Niri, and other compositors that expose the protocol. Its presence
does not prove that policy will authorize the client; every send operation can
still fail and then uses clipboard fallback.

## Rationale

GNOME and KDE normally require the portal/libei permission path. wlroots and
Smithay compositors commonly expose the unstable virtual-keyboard manager.
No compositor-name allowlist can capture vendor and point-release differences,
so the sink probes the actual session. Clipboard remains the safest path when
neither native channel is available.

## Consequences

- The first automatic delivery can wait up to ten seconds while the user
  answers a portal consent dialog.
- `eitype`, `wrtype`, and `libxkbcommon` are runtime/build dependencies.
- Restore tokens and transcript text are never logged by nvstt.
- Host testing is still required for GNOME, KDE, Sway, Hyprland, and Niri.
- uinput is intentionally not enabled; it would bypass compositor policy.
