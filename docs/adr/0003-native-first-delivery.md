# Use native-first delivery with explicit fallbacks

The first release tries XDG portal/libei or compositor-supported Wayland input
paths before using clipboard delivery. If automatic delivery is unavailable,
the daemon copies the transcript, reports the delivery failure, and never
claims that it typed the text. Broad `uinput` injection remains an explicit
opt-in fallback.

## Considered options

- Requiring `uinput` would improve coverage but would add broad system access
  and installation risk.
- Making the daemon a configured input method would improve text insertion in
  some fields but would conflict with existing IMEs and miss other clients.

## Consequences

- Backend selection must use runtime capability checks.
- Portal permission denial is a normal, reportable state.
- GNOME, KDE, and wlroots paths can evolve independently behind one delivery
  boundary.
