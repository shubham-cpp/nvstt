# Rust system tray library options

Verification date: 2026-10-03. Scope: library and protocol research. No application code changed. No tray prototype ran.

## Recommendation

Use `ksni` directly for a Linux daemon. It provides the requested icon changes and microphone submenu through D-Bus. It does not require a window, GTK, or libappindicator. Its dependency list uses `zbus`, Serde, and Rust async libraries. This is the leanest fit among the options below. This recommendation concerns integration cost, not measured memory or binary size. [ksni dependencies](https://docs.rs/crate/ksni/latest), [ksni manifest](https://raw.githubusercontent.com/iovxw/ksni/master/Cargo.toml).

`tray-icon` now offers a KSNI backend too. Older advice that it always requires GTK is out of date. Consider this alternative if nvstt later needs Windows or macOS support. [tray-icon 0.26.0](https://docs.rs/tray-icon/0.26.0/tray_icon/).

## How the tray works

The application publishes `org.kde.StatusNotifierItem` and a `com.canonical.dbusmenu` menu on the desktop session bus. A StatusNotifierWatcher registers the item. A desktop panel displays its icon and renders the menu. The application does not draw a Wayland popup itself. ksni implements both D-Bus interfaces. [ksni interface source](https://raw.githubusercontent.com/iovxw/ksni/master/src/dbus_interface.rs), [ksni service source](https://raw.githubusercontent.com/iovxw/ksni/master/src/service.rs).

This is desktop protocol support accessed through a Rust crate. Rust's standard library does not supply this tray interface. A full GUI framework is unnecessary for the requested behavior.

## Available behavior with ksni

| Requirement | Supported mechanism | Application decision |
| --- | --- | --- |
| Idle, listening, transcribing, typing, failure icons | `icon_name()` or `icon_pixmap()` plus `Handle::update()` | Define five application states and one icon per state. |
| Microphone submenu | `SubMenu` containing a `RadioGroup` | Store stable device identities separately from menu positions. |
| Show the active microphone | `RadioGroup.selected` | Change the selection after the capture controller accepts the request. |
| Additional controls | Standard items, checkmark items, separators, disabled items | Add start/stop, refresh devices, copy transcript, or quit when needed. |
| Status text | Title, tooltip, disabled menu item | Keep status understandable when color differences are hard to see. |
| Left click | `activate()` or `MENU_ON_ACTIVATE` | Prefer opening the menu initially. |
| Middle click or wheel | `secondary_activate()` and `scroll()` | Optional shortcuts, subject to host support. |

The APIs expose these capabilities directly. They do not switch audio devices themselves. [Tray trait](https://docs.rs/ksni/latest/ksni/trait.Tray.html), [menu types](https://docs.rs/ksni/latest/ksni/menu/), [submenu](https://docs.rs/ksni/latest/ksni/menu/struct.SubMenu.html), [radio group](https://docs.rs/ksni/latest/ksni/menu/struct.RadioGroup.html), [update handle](https://docs.rs/ksni/latest/ksni/struct.Handle.html).

The protocol status has three values: `Passive`, `Active`, and `NeedsAttention`. These do not limit the application to three icons. Keep the idle icon `Active` so the microphone menu remains accessible. Hosts can hide `Passive` items. Use a custom failure icon, with `NeedsAttention` if appropriate. [ksni status](https://docs.rs/ksni/latest/ksni/enum.Status.html), [Waybar passive visibility setting](https://raw.githubusercontent.com/Alexays/Waybar/master/man/waybar-tray.5.scd).

Embedded pixmaps use ARGB32 bytes in network byte order. `icon_pixmap()` accepts several sizes. A small set of embedded images avoids icon-file access across the Distrobox boundary. This is a design recommendation, not a tested packaging result. Theme names also work, but the host must find the matching icon. [Icon format and embedded example](https://docs.rs/ksni/latest/ksni/struct.Icon.html), [Tray icon properties](https://docs.rs/ksni/latest/ksni/trait.Tray.html).

Right click commonly opens the host-rendered menu. The application must not depend on receiving a raw right-click event. The tray-icon KSNI backend documents that the host handles right clicks itself. Click bindings and menu presentation vary by desktop. Animation, attention effects, and tooltips also depend on the host. Start with static state icons. [tray-icon event behavior](https://docs.rs/tray-icon/latest/src/tray_icon/lib.rs.html), [ksni attention properties](https://docs.rs/ksni/latest/ksni/trait.Tray.html).

## Runtime and event handling

ksni uses Tokio by default. It also supports `default-features = false` with `async-io`, or a `blocking` API. Keep the tray service alive throughout the daemon lifetime. Feed application state snapshots into its update handle. [ksni runtime choices](https://docs.rs/ksni/latest/ksni/).

Menu callbacks are synchronous. Send a command to the capture controller and return immediately. Do not reopen a microphone, run transcription, or wait for text insertion inside a callback. ksni explicitly warns that blocking callbacks freeze the menu. Its real-world example sends requests through a channel and updates selection after work completes. [radio callback contract](https://docs.rs/ksni/latest/ksni/menu/struct.RadioGroup.html), [real-world example in Handle docs](https://docs.rs/ksni/latest/ksni/struct.Handle.html).

Choose Tokio defaults when the dependency tree uses Tokio consistently. The upstream README documents a zbus feature-unification panic with certain mixed executor arrangements. Its alternative is `async-io`, which works with a Tokio application too. Inspect the resolved dependency tree before selecting features. [ksni runtime compatibility note](https://github.com/iovxw/ksni#note-on-tokio).

## Missing or restarted panels

Initial startup normally returns an error if the watcher is absent. `assume_sni_available(true)` lets the service wait for a desktop that starts later. It treats missing-watcher and missing-host startup conditions as recoverable. An absent tray host can still mean the icon never appears. [service builder](https://docs.rs/ksni/latest/ksni/struct.TrayServiceBuilder.html).

ksni watches changes in the watcher's bus owner and registers the item again when the watcher returns. `watcher_offline()` can enable another status channel; its default returns `true` and keeps the service running. `watcher_online()` reports recovery. This covers watcher restarts. It does not establish recovery after the entire session bus restarts. [service loop](https://raw.githubusercontent.com/iovxw/ksni/master/src/service.rs), [watcher callbacks](https://raw.githubusercontent.com/iovxw/ksni/master/src/lib.rs), [offline reasons](https://docs.rs/ksni/latest/ksni/enum.OfflineReason.html).

The design should keep dictation usable if no tray host exists. Retain hotkeys, CLI control, and an error notification fallback. The tray must connect to the host desktop session bus. ksni uses `zbus::connection::Builder::session()`, so the launch environment matters in Distrobox. [session connection source](https://raw.githubusercontent.com/iovxw/ksni/master/src/service.rs).

## Desktop support

| Desktop | Required host |
| --- | --- |
| KDE Plasma | Plasma provides a StatusNotifierWatcher on the session bus. Confirm the tray item is visible in panel settings. |
| GNOME | An enabled AppIndicator/KStatusNotifierItem extension provides support. Do not assume every installation includes it. |
| Sway, Hyprland, other wlroots sessions | A panel with SNI support, such as Waybar with its tray module enabled. The compositor name alone does not establish support. |
| Quickshell-based panels | The shell configuration must display `SystemTray.items` and open each item's menu. Quickshell provides the tray service and menu APIs. |

These statements follow each host's own implementation or documentation. They establish available mechanisms, not an nvstt compatibility test. [Plasma watcher implementation](https://raw.githubusercontent.com/KDE/plasma-workspace/master/statusnotifierwatcher/statusnotifierwatcher.cpp), [GNOME extension](https://extensions.gnome.org/extension/615/appindicator-support/), [Waybar SNI host and watcher](https://raw.githubusercontent.com/Alexays/Waybar/master/include/modules/sni/tray.hpp), [Waybar tray configuration](https://raw.githubusercontent.com/Alexays/Waybar/master/man/waybar-tray.5.scd).

Quickshell exposes icons through `SystemTrayItem.icon` and menus through `display()`, `QsMenuAnchor`, or `QsMenuOpener`. The panel configuration controls its click bindings. nvstt can publish the same tray item and menu for Plasma, Waybar, and Quickshell. No separate nvstt plugin is required for each panel. This is the intended protocol compatibility, pending live tests. [Quickshell tray service](https://quickshell.org/docs/v0.3.1/types/Quickshell.Services.SystemTray/SystemTray/), [Quickshell tray item and menu API](https://quickshell.org/docs/v0.3.1/types/Quickshell.Services.SystemTray/SystemTrayItem/).

The `org.kde` interface names do not require a KDE desktop or KDE libraries. They are protocol names used by other tray hosts too. Quickshell registers its host through `org.kde.StatusNotifierWatcher`. Detect the protocol service rather than branching on the desktop name. [Quickshell host implementation](https://raw.githubusercontent.com/quickshell-mirror/quickshell/master/src/services/status_notifier/host.cpp).

## Alternatives and current versions

| Option | Verified version and license | Dependencies and fit |
| --- | --- | --- |
| Direct `ksni` | 0.3.6; Unlicense; declared Rust 1.80 | Pure Rust SNI and DBusMenu implementation, with `zbus` 5. Best Linux daemon fit. |
| `tray-icon` with KSNI | 0.26.0; MIT OR Apache-2.0; Rust 1.90 | Disable default features and enable `ksni`. Adds Muda 0.21 menu abstraction and a worker thread. Avoids GTK when GTK features remain disabled. |
| `tray-icon` default backend | Same crate | GTK 3 and libappindicator or libayatana-appindicator, plus an event loop. Fits an existing GUI better. |
| `libappindicator` bindings | 0.9.0; Apache-2.0 OR MIT | GTK 0.18 and C library bindings. Exposes GTK menus and icon setters. |
| `appindicator3` Ayatana bindings | 0.3.0; MIT | Links libayatana-appindicator3 by default. Requires its development package and GTK 0.18. |
| Direct `zbus` | 5.19.0; MIT; Rust 1.87 | D-Bus connection, service, signals, and proxies. The application must implement SNI, menu layout/events, change signals, and registration recovery. |

Version and dependency evidence: [ksni manifest](https://raw.githubusercontent.com/iovxw/ksni/master/Cargo.toml), [tray-icon release history](https://docs.rs/crate/tray-icon/latest), [tray-icon published manifest](https://docs.rs/crate/tray-icon/latest/source/Cargo.toml), [Muda published manifest](https://docs.rs/crate/muda/latest/source/Cargo.toml), [libappindicator manifest](https://raw.githubusercontent.com/tauri-apps/libappindicator-rs/master/Cargo.toml), [appindicator3 manifest](https://raw.githubusercontent.com/rehar/appindicator3/master/Cargo.toml), [Ayatana binding setup](https://github.com/rehar/appindicator3), [zbus published manifest](https://docs.rs/crate/zbus/latest/source/Cargo.toml).

Declared Rust versions are not guarantees for a newly resolved dependency tree. ksni allows any compatible zbus 5 release. Current zbus 5.19.0 declares Rust 1.87. Check the complete resolved tree when adding a dependency.

Muda offers submenus and check items. Its documented menu API does not expose ksni's dedicated radio-group type. A single selected microphone can use check items with application-managed exclusivity. With tray-icon's KSNI feature, Muda supplies thread-safe menu snapshots. [Muda menu API](https://docs.rs/muda/latest/muda/), [tray-icon KSNI features](https://docs.rs/crate/tray-icon/latest/source/Cargo.toml).

Ayatana now marks its older GTK library obsolete and directs new work to `libayatana-appindicator-glib`. The replacement avoids GTK and libdbusmenu, but remains a C/GLib library. This research did not verify a suitable Rust binding for that replacement. It has no clear advantage over direct ksni for nvstt. [older Ayatana library](https://github.com/AyatanaIndicators/libayatana-appindicator), [GLib replacement](https://github.com/AyatanaIndicators/libayatana-appindicator-glib).

Direct zbus avoids an extra tray wrapper but transfers protocol maintenance to nvstt. It is technically feasible. It is not the preferred first implementation because ksni already supplies the needed menu and recovery behavior. [zbus service API](https://docs.rs/zbus/latest/zbus/), [ksni D-Bus interfaces](https://raw.githubusercontent.com/iovxw/ksni/master/src/dbus_interface.rs).

## Verification still required

- Show all five icons in the user's actual host panel, including high DPI and the idle state.
- Open the microphone submenu with right click and confirm radio selection updates correctly.
- Repeat icon, menu, radio selection, and host-restart checks on Plasma, Waybar, and a Quickshell panel with tray menus configured.
- Confirm the Fedora Distrobox process reaches the host session bus, not a container-only bus.
- Restart or disable the panel watcher and confirm re-registration and fallback behavior.
- Confirm microphone changes and device removal update the menu without blocking it.
- Measure memory, dependency growth, and release binary size if lean footprint is an acceptance requirement.

No Cargo dependency resolution, build, desktop interaction, or microphone-switch test ran for this library report.
