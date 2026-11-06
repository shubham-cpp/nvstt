# System tray and runtime microphone implementation plan

Date: 2026-10-03.
Status: implementation complete. Automated and available Plasma checks passed. The broader live-environment gate remains partial.
Source baseline: `68fa5285929de590a1793c2df909f06798a760fb`.

This document is the task list and behavior specification for the first release. Check a task only after its work, validation, and reviews pass. Record any scope change here before implementing it.

## Goal and scope

Give nvstt a generic Linux system tray item with changing activity icons and a microphone submenu. Users can select an input between dictations without restarting the daemon.

Use `ksni` to publish a StatusNotifierItem and D-Bus menu. Target Plasma, Waybar, and Quickshell panels with tray support. Keep audio selection independent of the desktop shell.

The first release includes:

- Five main icons: ready, listening, transcribing, typing, and error.
- Startup feedback, truthful clipboard fallback, and retained failure feedback.
- An input submenu with one selected preference and an explicit refresh action.
- Runtime selection through both the tray and CLI.
- A responsive tray and status command during finalization and delivery.
- Graceful behavior when the tray host is absent or restarts.
- A documented Distrobox launch arrangement that reaches the desktop session.

The first release excludes:

- Switching inputs during an active dictation.
- Remembering a tray selection after daemon restart. Selection is runtime-only initially.
- A separate tray process, settings window, or custom desktop widget.
- Per-desktop implementations, KDE control commands, or system-default input changes.
- Tray actions that stop and automatically deliver text, or retry delivery.
- Icon animation, input level meters, model selection, and transcript display in the tray.
- Changes to recognizer models, speech-gate policy, history retention, or saved-audio format.

These exclusions limit the initial implementation. They do not prohibit a later extension with its own specification.

## Evidence and starting conditions

Read these reports before implementation:

- [Library options](../../research/16-system-tray-library-options.md).
- [Tray and microphone feasibility](../../research/17-system-tray-and-runtime-microphones.md).
- [Current domain language](../../../CONTEXT.md).
- [Dictation integrity decision](../../adr/0013-dictation-integrity.md).
- [Recent audio decision](../../adr/0014-retain-recent-dictation-audio.md).

The previous probes established host tray access and input enumeration. They did not establish live capture reliability or tray rendering.

At the baseline:

- `DaemonState` already has starting, idle, listening, finalizing, delivering, and error variants.
- The recorder opens CPAL's default input for each dictation.
- Stop joins the recognition worker, saves audio, and performs delivery before replying.
- Delivery-start has no notification event. Failure paths can reset the daemon to idle.
- IPC supports toggle, cancel, status, and history. It has no input commands.
- The Fedora Distrobox has a separate session bus. The host bus is reachable through a different socket path.
- Existing working-tree edits include `README.md` and untracked deployment, research, and tooling files. Preserve them.

Source findings use the codebase-memory graph and targeted source reads. Coverage checks recorded matching metadata and no gaps for the relied-on Rust files. Recheck coverage when implementation starts. A clean coverage result does not prove every path was indexed correctly.

Do not copy historical test counts or failures from older plans. Task 0 records the current baseline.

## Behavior contract

### Terms

| Term | Meaning |
| --- | --- |
| Activity | What the daemon is doing now. |
| Last outcome | The result of the latest completed attempt. It can outlive its activity. |
| Input preference | Follow system default, or a specific host-qualified device ID. |
| Active input | The input resolved and opened for the current dictation. |
| Input catalog | The latest discovered inputs, with stable identifiers and display labels. |
| Tray host | The panel that displays the published icon and menu. |
| Host availability | Whether the tray protocol currently reports a usable host. This does not prove the icon is visible. |
| Typing icon | The user-facing icon for delivery activity. Delivery can include authorization and fallback attempts. |

Use the existing term `Delivery` in domain code. `Typing` is an icon label, not a new delivery mechanism.

### Icons and outcome rules

| Situation | Icon and message | Required behavior |
| --- | --- | --- |
| Startup | Ready shape, `Starting` message | Disable input selection until initialization completes. Do not claim readiness. |
| Healthy idle | Ready | Show selected input and availability. |
| Capture started | Listening | Show the active input. Publish only after stream start succeeds. |
| Stop accepted | Transcribing | Keep this activity through finalization, text preparation, and saved-audio work. |
| Delivery begins | Typing | Publish before any portal wait or text sink call. |
| Automatic delivery succeeds | Ready, last result `Delivered` | Keep the existing single final delivery contract. |
| Clipboard fallback | Ready, last result `Copied; automatic delivery unavailable` | Do not report automatic delivery success. No sixth icon is required. |
| No speech | Ready, last result `No speech` | No delivery or text-history entry. Keep existing stopped-audio behavior. |
| Cancellation | Ready, last result `Canceled` | No delivery, text-history entry, or saved audio. |
| Failed start, capture, transcription, or delivery | Error, with named stage and safe hint | Keep failure visible when the daemon becomes idle. |
| Input selection fails | Keep current activity; show selection error in the menu | Do not replace the previous preference or erase the last dictation outcome. |
| History or audio save warns | Keep the primary activity/outcome; show a short warning | Do not turn successful transcription or delivery into failure. |
| Clear last error | Ready if idle and available | Do not hide a continuing initialization or device-availability problem. |
| New dictation starts successfully | Listening | Clear the retained prior attempt failure. A rejected start does not clear it. |

Active activity takes precedence over a retained failure icon. A persistent startup problem remains visible until it is resolved. Track distinct errors explicitly instead of storing all problems in one message string. Show full backend reasons in CLI status. Tray labels use safe stage hints because raw errors can contain private text.

Do not delay real work to make brief icons visible. Hosts control repaint timing. Use the last outcome for durable feedback.

### Input selection rules

- Start with `Follow system default`. Resolve it at each dictation start.
- Accept a new preference only while idle and initialized.
- Validate requests in both command admission and the controller.
- Resolve a specific ID again immediately before stream creation.
- Reject missing, unsupported, busy, or invalid selections without changing the prior preference.
- A successful selection means the preference was accepted. It does not prove a stream can open later.
- Keep the preference visible as unavailable if its device disappears. Do not silently substitute another microphone.
- Selection affects nvstt only. Never change the system input default.
- Never persist a menu index, device display name, or transient server object number as the selected identity.
- Keep virtual microphone sources available. Label playback monitors separately from normal microphone sources.
- Reject selection while listening, finalizing, or delivering. Do not defer it silently until after delivery.
- Reset selection to `Follow system default` when the daemon restarts. Document this behavior.

### Tray menu

```text
nvstt: <activity>                         disabled status text
Input: <selected or active input>         disabled status text
Microphone >
    Follow system default                radio item
    <available physical and virtual inputs>
    Other capture sources >              optional monitor/unknown group
    Refresh inputs
Cancel dictation                         enabled while listening
Last result: <short outcome>              disabled status text
Clear last error                         enabled when a retained error can clear
```

Use one logical selection across all input groups. If the panel library cannot represent a radio group across submenus, use one flat radio group with clear source labels. Do not sacrifice exclusivity for grouping.

No transcript text belongs in tooltips or menu labels. Keep left-click behavior menu-oriented. Hosts control right-click behavior and placement.

### Notification policy

With a reported tray host available, suppress routine phase notifications. Keep actionable initialization, capture, transcription, and delivery failure notifications. Keep clipboard fallback feedback because the user must paste the text.

With no reported tray host, keep the existing notification feedback. A disabled tray uses this same fallback. Host availability cannot detect a user hiding the item in panel settings. Document that limit.

## Architecture and file map

Use one daemon process and one controller that owns mutable dictation state. Keep capture callbacks and the recognition worker in their existing roles.

```text
CLI / hotkey ─> async IPC handler ─┐
                                 ├─> command admission ─> controller thread
Tray menu ────────────────────────┘                           │
                                                            ├─> recorder
                                                            ├─> recognition worker
                                                            ├─> history / recent audio
                                                            └─> delivery
                                                                  │
                          status snapshot <───────────────────────┘
                                │
                     ┌──────────┴───────────┐
                     │                      │
               IPC status read        ksni tray update
```

The controller owns the `Daemon`. The async handler owns connection handling and cached snapshot reads. Tray callbacks submit bounded commands and return immediately. No tray callback invokes a recorder or text sink.

Command admission must reserve a mutation before handing it to the controller. Reject competing mutations while that command runs. This prevents a second toggle or input selection from waiting and executing after a long delivery. Release the reservation on success, failure, and worker termination. The controller also checks activity and request generation.

Use a small published snapshot for current state. Include a revision, activity, readiness, last outcome, selected input, active input, and input availability. Increment a session generation on transitions that invalidate pending menu actions. Distinguish a harmless presentation refresh from a session change.

The controller publishes before blocking finalization and delivery. The async handler can answer status from that snapshot. History reads can remain serialized through the controller. Do not add a second writer or an unsynchronized history cache.

Use a private test adapter where work actually varies. Avoid a generic plugin framework. Keep device discovery and capture selection behind a small audio-input interface, with a deterministic test adapter and the CPAL adapter.

| File or module | Planned responsibility |
| --- | --- |
| `src/domain.rs` | Typed input preference and activity/outcome data shared with IPC. |
| `src/app.rs` | Existing dictation transitions and explicit presentation publication. |
| New `src/controller.rs` | Serialized ownership, command admission coordination, completion, shutdown. |
| New `src/audio_input.rs` | Input catalog, selection validation, CPAL device resolution. |
| `src/recorder.rs` | Open the resolved input and keep the existing capture integrity contract. |
| `src/ipc.rs` | Additive input commands and snapshot fields. |
| `src/main.rs` | Input CLI commands and a daemon tray-disable option. |
| New `src/tray.rs` | ksni lifetime, icon/menu projection, host availability, callback routing. |
| New tray asset directory | Embedded, authored icon pixmaps at useful sizes. |
| `src/notifier.rs` | Notification policy using typed activity/outcome and host availability. |
| `src/lib.rs` | Export only modules required by callers. Keep implementation details private. |
| `Cargo.toml`, `Cargo.lock`, `THIRD_PARTY_NOTICES.md` | Dependencies, runtime features, lockfile, licenses. |
| `README.md`, `contrib/`, deployment documentation | Commands, tray setup, supported environments, launch instructions. |

File names describe intended ownership. Adjust them if an existing module can own the behavior cleanly. Record the reason. Do not introduce wrappers solely to match this table.

## Global dos and don'ts

### Do

- Build, test, format, and inspect Cargo dependencies through the `Fedora` Distrobox.
- Prefix shell commands with `rtk`. Use `rtk proxy` for commands without an RTK filter.
- Use separate target directories when building review binaries. The current daemon runs an existing release binary.
- Preserve user changes. Stage explicit files only if commits are requested later.
- Keep final-only delivery, capture loss rejection, queue bounds, and stop-tail draining.
- Preserve cancel, no-speech, history, and recent-audio behavior.
- Detect protocol availability. Test the same tray item across supported panels.
- Use bounded queues, explicit outcomes, and test barriers for concurrency checks.
- Record measured results and environment details. Separate synthetic tests from live desktop tests.
- Keep personal audio and live-test reports outside Git.

### Don't

- Use the `rm` command. Use `trash` or `gio trash` for file removal.
- Branch on `XDG_CURRENT_DESKTOP=KDE` or require KDE libraries and configuration tools.
- Set global microphone defaults through `wpctl`, `pactl`, or desktop control tools.
- Hold a tray lock during device discovery, recording, inference, notification calls, or delivery.
- Run blocking work inside ksni callbacks or an async executor task without an explicit blocking arrangement.
- Accumulate stale mutating commands during a busy phase.
- Infer delivery-start from transcript availability.
- Reset the error icon merely because the daemon returned to idle.
- Retry automatic delivery, deliver partial text, or type from a tray stop action.
- Rewrite unrelated configuration or silently remember runtime-only choices.
- Install, replace, or restart the user's active daemon as part of routine task verification.
- Report protocol support, enumeration, or unit tests as proof of live compatibility or capture correctness.

## Review procedure for every task

Each task below has a review gate. Use this procedure for every gate, including documentation tasks.

1. Record the task ID, implementation base commit, affected files, and behavior clauses.
2. Review the complete task diff. Include unstaged and untracked task files, not only committed changes.
3. Run focused checks. Run broader checks when an interface, dependency, or concurrency change requires them.
4. Review code quality and behavior separately. Do not let one passing review mask the other.
5. Record findings with file location, failure scenario, severity, and proposed fix.
6. Fix blocking findings. Review the updated diff and rerun affected checks.
7. Mark the gate complete only when evidence and unresolved limits are recorded.

The code-quality review checks module ownership, small interfaces, duplication, names, dependency direction, locking, resource cleanup, and error paths. Treat code smells as judgments, not automatic failures. Repository instructions take precedence.

The behavior review compares the change with this plan. Check missing requirements, incorrect behavior, compatibility changes, and scope growth. Cite the relevant clause.

Concurrency, capture, IPC, and dependency tasks need an independent reviewer when available. Two reviewers can work separately on the two review axes. If an independent reviewer is unavailable, record a self-review explicitly. Do not claim an independent review occurred.

For a committed task, compare the recorded base with the task's final commit. For uncommitted work, review `git diff <base>` and inspect newly created files directly. If using the installed code-review skill, give it the fixed point and this plan as the specification. Do not assume its committed-only diff includes working-tree edits.

Use this record under each completed task:

```text
Task:
Base / final revision:
Files:
Checks and results:
Code-quality reviewer / findings / disposition:
Behavior reviewer / findings / disposition:
Live evidence or untested limits:
Gate: passed / blocked / pending
```

Do not advance past a failed prerequisite. A blocking finding includes audio loss, duplicate delivery, wrong input, stale command execution, a frozen tray, leaked private content, or a broken existing command. Record smaller follow-ups rather than leaving hidden TODO comments.

## Task sequence

| Task | Work | Prerequisites |
| --- | --- | --- |
| 0 | Record baseline and acceptance contract | None |
| 1 | Validate dependency and audio backend choices | 0 |
| 2 | Define typed snapshots and presentation rules | 0 |
| 3 | Keep controller work and status reads independent | 2 |
| 4 | Implement input discovery and device resolution | 1, 2 |
| 5 | Add runtime selection to capture and controller | 3, 4 |
| 6 | Add input CLI and additive IPC | 5 |
| 7 | Implement tray icons and host lifetime | 1, 2, 3 |
| 8 | Connect the input menu and safe actions | 5, 6, 7 |
| 9 | Apply notification and failure feedback policy | 7, 8 |
| 10 | Document and validate desktop session launch | 6, 7, 9 |
| 11 | Run integrated regression and live validation | 0 through 10 |
| 12 | Complete documentation, final review, and release checklist | 11 |

This is a dependency map. Implement sequentially by default. Independent work can proceed in parallel only with clear file ownership and coordination.

### Task 0: Record the baseline and freeze the acceptance contract

Files: this plan, task-local validation notes outside personal audio storage.

Todo:

- [x] Record branch, HEAD, working-tree changes, and current deployment paths.
- [x] Record the active daemon binary and socket without stopping or replacing it.
- [x] Confirm the runtime-only selection scope and menu behavior specified above.
- [x] Record available live-test environments: Plasma, Waybar, Quickshell, and optional GNOME.
- [x] Run current formatting, tests, and Clippy through Fedora using a separate target directory.
- [x] Record existing failures with exact output and ownership. Do not relabel them as feature regressions.
- [x] Establish a private test location and a separate runtime directory for test daemons.

Do: use a disposable working tree if it simplifies separation from existing user changes.

Don't: run test daemons against the production socket, history, model install path, or recording store.

Validation: baseline results are reproducible. No active daemon, desktop configuration, or input route changed.

Review focus: compare the scope with the user's generic desktop requirement. Check that prior research limits remain explicit.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: baseline and scope are recorded without implementation claims.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: this plan; private baseline notes.
- Checks: Baseline CPU suite passed 155 tests; three ignored. Formatting and Clippy passed. A missing Rust formatting toolchain was an environment issue. Mise supplied Rust 1.99.0.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: The production PID and socket were recorded. The previous binary was preserved. Seven WAVs and references were copied to a private directory. Existing README and deployment edits were preserved.
- Gate: passed for implementation and recorded validation scope.


### Task 1: Validate dependencies and the microphone backend

Files: `Cargo.toml`, `Cargo.lock`, `THIRD_PARTY_NOTICES.md`; disposable dependency probes outside the application source.

Todo:

- [x] Resolve `ksni` 0.3.6 and inspect its complete runtime feature tree.
- [x] Check zbus executor features against existing notification and delivery dependencies.
- [x] Add Tokio's `sync` and `time` features explicitly if channels, backoff, and request deadlines use them.
- [x] Evaluate CPAL 0.18.2 with `pulseaudio` while retaining the existing recorder interface.
- [x] Confirm explicit host selection and saved IDs do not silently change host when initialization fails.
- [x] Verify enumeration, device ID lookup, input configuration queries, and device disappearance behavior.
- [x] Determine how the catalog distinguishes playback monitors, virtual inputs, and unknown sources using supported metadata.
- [x] Record any required supplemental metadata access. Avoid importing an audio framework just for labels.
- [x] Keep ALSA fallback explicit. A pinned PulseAudio preference must not silently resolve to an ALSA device.
- [x] Measure or record baseline dependency count and release binary size for later comparison.
- [x] Update license notices for dependencies that require them.

Do: compile a minimal tray and audio probe against the actual locked dependencies before refactoring the daemon.

Don't: assume CPAL feature enablement changes only enumeration. It can change the default capture host and runtime behavior.

Validation: dependency resolution and compilation pass in Fedora. The probe reports the actual selected host and known input IDs. A real stream smoke test remains distinct from enumeration and requires a private test setup.

Review focus: added dependency necessity, executor compatibility, reproducible lockfile, build/runtime dependencies, and honest source classification.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: chosen backend and metadata route are documented and compile successfully.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: Cargo.toml; Cargo.lock; THIRD_PARTY_NOTICES.md.
- Checks: ksni 0.3.6, CPAL 0.18.2 PulseAudio, and pulseaudio 0.3.1 compiled in Fedora. The zbus feature tree confirms existing Tokio and async-io features coexist. The lockfile adds seven packages, from 371 to 378.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: The real catalog listed three microphone sources and two monitors. Metadata uses the existing Rust PulseAudio protocol dependency. No GTK, Qt, or native libpulse dependency was added.
- Gate: passed for implementation and recorded validation scope.


### Task 2: Define typed activity, outcomes, and presentation

Files: `src/domain.rs`; a small presentation module if needed; focused tests.

Todo:

- [x] Define input preference, input ID, catalog entry, active input, and availability types.
- [x] Define retained attempt outcome separately from activity, command errors, and readiness errors.
- [x] Define a small snapshot with revision and session generation fields.
- [x] Specify the one-way projection from the snapshot to icon, tooltip, and enabled menu actions.
- [x] Map every row in the icon/outcome table, including startup failure and failed start.
- [x] Define how selected-but-missing inputs remain visible and how the default input is labeled.
- [x] Keep existing daemon state and existing serialized fields compatible where possible.
- [x] Add behavior tests for state precedence, retained failure, clipboard fallback, cancellation, warnings, and successful-start clearing.
- [x] Test that tooltips and menu text cannot include transcript content from the snapshot.

Do: centralize presentation decisions so the tray, notifications, and status do not each invent separate state logic.

Don't: add a second dictation state machine inside the tray or use one untyped string for unrelated failure kinds.

Validation: pure behavior tests pass without a microphone, model, desktop, or D-Bus session.

Review focus: complete outcome mapping, compatible serialization, meaningful types, and no speculative status abstractions.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: snapshot and presentation rules cover all required states.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/domain.rs; src/tray.rs; src/tray_icons.rs.
- Checks: State precedence, failure retention, clipboard feedback, optional old status fields, warning retention, and private-text exclusion tests passed.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Starting uses the ready shape and a Starting label. Cached unavailable inputs remain explicit. Failure labels deliberately show safe stage hints. Full reasons remain in CLI status.
- Gate: passed for implementation and recorded validation scope.


### Task 3: Keep controller work and status reads independent

Files: `src/app.rs`, new `src/controller.rs`, `src/ipc.rs`, `src/lib.rs`; controller integration tests.

Todo:

- [x] Move mutable daemon ownership to one dedicated controller thread or an equally explicit blocking arrangement.
- [x] Publish starting state before model loading. Let tray registration and status reads run while initialization proceeds.
- [x] Construct or transfer Send-compatible recorder and delivery objects with clear lifetime ownership.
- [x] Keep command completion replies associated with their original request.
- [x] Add a bounded admission queue and a mutation reservation.
- [x] Reject competing mutations immediately while initialization, stop, or delivery owns the reservation.
- [x] Validate activity and request generation in the controller before applying an input change or cancellation.
- [x] Publish listening, finalizing, and delivering before their corresponding slow work.
- [x] Publish readiness and outcome changes on all success, error, panic, and cancellation paths.
- [x] Serve status from a published snapshot while the controller is blocked.
- [x] Handle IPC connections independently so a slow client cannot block tray updates or other status requests.
- [x] Bound request-line reads and malformed-input handling. Preserve the private Unix socket checks.
- [x] Keep history reads serialized. Document their behavior while delivery runs.
- [x] Define shutdown admission, capture cancellation, controller join, tray termination, and socket cleanup order.
- [x] Prevent any new delivery after shutdown begins. Do not claim an already-started external delivery can always be canceled.

Do: use channel barriers to hold finalization and delivery in tests. Assert status and tray consumers still progress.

Don't: hold shared mutexes around a worker join or text sink. Do not spawn one unordered daemon mutation per request.

Validation: test concurrent toggle/input requests, queue full, dropped reply receivers, stalled IPC clients, controller panic, and shutdown. Assert no late selection, accidental restart, duplicate finalization, or duplicate delivery.

Review focus: single ownership, reservation release, stale requests, backpressure, blocking operations, and resource cleanup. This task requires independent review when available.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: status stays responsive and mutation ordering is deterministic under blocked work.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/app.rs; src/controller.rs; src/ipc.rs.
- Checks: Barrier tests held finalization and delivery. Status returned within the 100 ms test deadline. Competing mutations were rejected. Shutdown prevented later delivery. Controller panic preserved error status.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: The reservation now releases before completion. This fixes an immediate follow-up race. An incomplete live IPC client did not block status. The isolated socket was removed after shutdown.
- Gate: passed for implementation and recorded validation scope.


### Task 4: Implement the input catalog and device resolver

Files: new `src/audio_input.rs`, relevant domain types, `src/lib.rs`; deterministic catalog tests.

Todo:

- [x] Implement the small audio-input interface with a CPAL adapter and deterministic test adapter.
- [x] List supported input sources with host-qualified IDs, display labels, and availability.
- [x] Distinguish playback monitors from microphone and virtual source choices using the route verified in Task 1.
- [x] Classify uncertain entries as unknown. Do not guess from translated display labels.
- [x] Exclude null and known non-capture placeholders from normal microphone choices.
- [x] Deduplicate by identity. Keep duplicate display names distinguishable.
- [x] Resolve default and pinned preferences using a fresh backend view.
- [x] Recreate snapshot-based hosts when refreshing. Do not keep stale CPAL devices as permanent selection state.
- [x] Preserve the prior catalog on refresh failure and mark it stale. Reject unavailable selections explicitly.
- [x] Handle no inputs, inaccessible server, disconnect, malformed IDs, and unsupported host IDs.
- [x] Keep discovery off tray callbacks and capture callbacks.

Do: keep the input ID separate from its display label and menu ordering.

Don't: use transient PulseAudio indexes or PipeWire object serials as persistent application identity.

Validation: cover reorder, duplicate names, virtual inputs, monitors, selected input disappearance, empty catalogs, and server failure. Check live discovery separately in Fedora.

Review focus: metadata validity, refresh semantics, device lifetime, source classification, and absence of global audio side effects.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: catalog and resolver produce correct identities and failure results.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/audio_input.rs; src/domain.rs; src/app_tray_tests.rs.
- Checks: Fresh default resolution, pinned-device loss, monitor classification, and recovery tests passed. The live catalog identified five sources.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Names remain presentation labels. Selection uses host-qualified IDs. Duplicate names receive an ID suffix in the menu. Physical unplug and server-loss capture tests remain untested.
- Gate: passed for implementation and recorded validation scope.


### Task 5: Add runtime input selection to capture

Files: `src/recorder.rs`, `src/app.rs`, `src/controller.rs`, `src/audio_input.rs`; existing capture and daemon tests.

Todo:

- [x] Store the runtime preference in controller-owned state.
- [x] Accept selection only while idle and initialized. Check again before mutation.
- [x] Validate a chosen ID before accepting it. Report selection success separately from capture success.
- [x] Resolve the preference at session start and pass the resolved input to the recorder.
- [x] Publish active input only after the stream starts successfully.
- [x] Keep the active input fixed for the entire dictation.
- [x] Keep default resolution at session start. Record backend-dependent default rerouting as a tested limitation.
- [x] Reject a missing pinned device without substituting another input.
- [x] Preserve format selection, mono downmix, queue capacity, integrity reporting, and stop-tail draining.
- [x] Preserve saved-audio rate and metadata format. Do not mix two capture rates into one recording.
- [x] Keep development transcript mode independent of real microphone availability.
- [x] Clear active input on finish/cancel while retaining the runtime preference.
- [x] Surface backend faults through the existing integrity contract. Report when faults become observable; do not claim instantaneous detection without implementing it.

Do: retain the current no-partial-delivery behavior after capture loss.

Don't: reopen a stream during listening or change resampling, VAD, and recognition defaults as part of this task.

Validation: select A, record, stop, select B, record again. Test missing A, failed stream start, backend error, overflow, stop failure, and cancellation. Verify failed sessions do not deliver partial transcripts.

Review focus: selected versus active input, race handling, format correctness, integrity regressions, and unchanged retention behavior. This task requires independent review when available.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: input changes apply only to a later session and preserve capture integrity.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/recorder.rs; src/app.rs; src/app_tray_tests.rs.
- Checks: All existing capture-integrity and stop-tail tests passed. Seven preserved 48 kHz clips passed full daemon replay. Saved samples matched exactly.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: All three real microphone sources opened and canceled. Busy selection was rejected. No test cancel created history or WAV entries. Missing pinned inputs did not substitute the default.
- Gate: passed for implementation and recorded validation scope.


### Task 6: Add additive IPC and input CLI commands

Files: `src/ipc.rs`, `src/main.rs`, relevant domain types; serialization and CLI tests.

Proposed command names:

```text
nvstt audio inputs [--json]
nvstt audio select --default
nvstt audio select --id <host-qualified-id>
nvstt daemon --no-tray
```

Todo:

- [x] Add typed list-input and select-input requests and responses.
- [x] Make input listing perform an explicit fresh catalog request, subject to busy handling.
- [x] Expose selected and active input, last outcome, and availability in additive status fields.
- [x] Add serde defaults for newly optional fields where old messages may omit them.
- [x] Preserve existing toggle, cancel, status, history, and model commands and exit behavior.
- [x] Add explicit machine-readable failure codes for the new input operations.
- [x] Test mutually exclusive `--default` and `--id` arguments.
- [x] Treat the device ID as data. Do not interpolate it into a shell command.
- [x] Report runtime-only selection in command help.
- [x] Add the daemon tray-disable option without disabling dictation or notification fallback.
- [x] Document mixed-version behavior. Older daemons need not understand new commands, but errors must be clear.

Do: use the same controller operation for CLI and tray input selection.

Don't: add a second selection implementation or change existing failure response shapes solely to simplify new commands.

Validation: old request fixtures decode, new messages round-trip, CLI conflicts fail cleanly, and existing commands remain functional. Exercise input commands against an isolated daemon and fake input provider.

Review focus: compatibility, typed error results, input validation, help text, and controller reuse.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: CLI and tray can share one tested input command path.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/main.rs; src/ipc.rs; src/app.rs.
- Checks: Existing commands remain valid. New audio list/select and clear-error requests use controller admission. The CLI help and supported selection flags were checked. The CPU binary also ran CLI help on the host.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Status fields are additive and have serde defaults. History and other controller requests can return busy during an accepted operation. Runtime preference is not persisted.
- Gate: passed for implementation and recorded validation scope.


### Task 7: Implement tray icons and host lifetime

Files: new `src/tray.rs`, tray assets, `src/lib.rs`, daemon startup wiring; tray adapter tests.

Todo:

- [x] Implement one ksni item with stable application identity and the configured menu behavior.
- [x] Author five small icons with distinct shapes. Do not rely on color alone.
- [x] Embed pixmaps at useful sizes and verify ARGB byte order and dimensions.
- [x] Project the snapshot to icon, title, tooltip, and protocol status.
- [x] Keep idle protocol status `Active` so hosts can keep the menu accessible.
- [x] Publish changes independently of controller joins and delivery waits.
- [x] Retain the item handle for the daemon lifetime and terminate it during orderly shutdown.
- [x] Handle initial watcher absence and delayed watcher startup.
- [x] Track watcher offline/online callbacks and re-registration.
- [x] Handle complete bus disconnection separately from watcher restart. Retry with bounded backoff or degrade cleanly.
- [x] Keep failure reasons short and exclude transcript, credentials, and portal restore tokens.
- [x] Make tray startup failure nonfatal to dictation.

Do: use embedded icons so panels do not need container file access or a matching host theme.

Don't: depend on KDE icon names, publish a Plasma-specific item, or force work delays for icon animation.

Validation: use a fake presentation consumer for mapping tests and an isolated session bus for registration tests. Live panel tests confirm rendering, scaling, watcher restart, and item cleanup.

Review focus: handle lifetime, retries, executor compatibility, truthful state, pixel encoding, and desktop independence.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: one item renders correctly and host loss does not interrupt controller work.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/tray.rs; src/tray_icons.rs.
- Checks: All five icons have distinct, nonempty ARGB data at 24, 32, and 64 pixels. Plasma registration and ready/listening screenshots passed. Private-bus late-host and watcher-restart checks passed.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Icons are small Rust-rendered shapes, cached once. This replaces separate image assets without adding an image loader. Whole session-bus replacement and high-DPI live rendering remain untested.
- Gate: passed for implementation and recorded validation scope.


### Task 8: Connect the microphone menu and safe actions

Files: `src/tray.rs`, controller command routing; menu interaction tests.

Todo:

- [x] Build the menu from the catalog and the published snapshot.
- [x] Bind each input action to its device ID, not its current position.
- [x] Capture the appropriate catalog/session generation for each action.
- [x] Submit callbacks nonblocking. Handle queue full and controller unavailable visibly.
- [x] Update selection only after the controller accepts it. Do not show optimistic success.
- [x] Disable selection and refresh while the relevant operation is busy; still enforce rules in the controller.
- [x] Keep a missing selected preference visible and allow choosing system default.
- [x] Keep physical and virtual sources available; label monitors/unknown capture sources accurately.
- [x] Wire refresh, cancel, and clear-error actions through the shared command path.
- [x] Reject stale cancellation actions so an old menu cannot cancel a newer dictation.
- [x] Show the last attempt outcome and selection-command errors without confusing them.
- [x] Keep menu generation and locks brief during concurrent icon updates.

Do: maintain one logical radio selection and tolerate catalog reordering.

Don't: use a tray click to invoke automatic delivery or silently queue a change during a busy phase.

Validation: click A while idle, click B while busy, reorder the catalog, unplug A, refresh, double-click, cancel with a stale generation, and simulate a full command queue. Confirm accepted selection and displayed selection agree.

Review focus: callbacks, lock ordering, stale item identity, menu state, acknowledgment, and focus-safe actions.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: every menu action uses the tested controller path and returns promptly.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/tray.rs; src/tray_dbus_tests.rs.
- Checks: Actual D-Bus events from old layouts failed after same-count device replacement and each cancel/start transition. Current cancel events succeeded.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Review found positional DBusMenu ID reuse. A fixed hidden submenu changes its child layout for each observed generation. ksni then invalidates old IDs. Callbacks remain nonblocking.
- Gate: passed for implementation and recorded validation scope.


### Task 9: Apply notification and retained failure feedback

Files: `src/notifier.rs`, `src/app.rs`, `src/tray.rs`; policy and daemon tests.

Todo:

- [x] Apply the notification policy from this document to typed events.
- [x] Suppress routine phase popups only when the tray reports a usable host.
- [x] Keep failure and clipboard fallback notifications actionable and concise.
- [x] Restore phase notifications when the watcher/host disappears or the tray is disabled.
- [x] Initialize notification policy before startup events so initialization failures remain visible.
- [x] Publish delivery-start before blocking text delivery.
- [x] Retain failures after idle reset and clear them only under the stated rules.
- [x] Keep save/history warnings separate from primary delivery and transcription outcomes.
- [x] Ensure notification failure never changes the dictation result.
- [x] Keep blocking notification calls off the async tray runtime and outside controller state locks.

Do: distinguish automatic delivery failure from successful clipboard fallback.

Don't: let host availability mean proven icon visibility or remove all fallback feedback.

Validation: compare host-online, host-offline, disabled tray, failed initialization, failed start, failed transcription, failed delivery, clipboard fallback, no speech, and cancellation. Verify duplicate phase notifications do not appear with the tray available.

Review focus: error precedence, readiness failure, outcome retention, notification transitions, and accurate clipboard messages.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: feedback is accurate with and without the tray host.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/notifier.rs; src/app.rs; src/app_tray_tests.rs.
- Checks: Policy tests passed for host-present and host-absent notifications. Failures and clipboard feedback remain enabled. Completion status keeps storage warnings and delivery failure.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Review found warning propagation gaps. History and audio warnings now merge into feedback. Successful refresh clears the previous input command error. Clear-error preserves successful outcomes and continuing availability problems.
- Gate: passed for implementation and recorded validation scope.


### Task 10: Document and validate desktop session launch

Files: `contrib/nvstt.service`, deployment examples if necessary, `README.md`, focused deployment documentation.

Todo:

- [x] Distinguish build-in-Fedora from run-on-host and run-inside-Fedora deployment.
- [x] Verify host runtime libraries for a host-launched review binary.
- [x] Verify the intended session bus for tray, notifications, and delivery portal calls.
- [x] Keep Distrobox host-bus paths in an explicit container launch example, not hardcoded application logic.
- [x] Preserve existing runtime, config, state, model, Wayland, PulseAudio, and PipeWire paths.
- [x] Do not change `XDG_RUNTIME_DIR` merely to fix tray bus access. It also controls sockets and other protocols.
- [ ] Validate existing service confinement against the new tray connection and test whether startup before a graphical session is handled.
- [ ] Ensure graphical logout/login and daemon restart do not create duplicate daemon instances.
- [x] Document enabling Waybar's tray and wiring Quickshell's tray icon/menu actions.
- [x] Document GNOME's extension requirement and headless fallback.
- [x] Validate launch examples in an isolated process. Leave the active service and user panel configuration untouched.

Do: discover protocol support at runtime and give users explicit launch instructions for their arrangement.

Don't: set KDE environment variables, install a desktop widget, or patch a user's shell configuration automatically.

Validation: the review daemon reaches the intended watcher and audio server in each deployment mode. Existing commands find the intended private socket. No production daemon or route was changed.

Review focus: host/container path confusion, service ordering, confinement, shared user identity, session lifecycle, and duplicate-process prevention.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: launch instructions are reproducible and do not depend on KDE.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: scripts/run_in_distrobox.sh; contrib/nvstt-distrobox.service; README.md.
- Checks: The host bus mapping reached Plasma from Fedora. Shell syntax passed. A private unit copy passed systemd-analyze with an existing launcher path. Normal and TERM-ignoring child shutdown probes passed.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Review found that killing Distrobox alone left its child alive. The launcher now verifies its child token, forwards TERM, waits, and escalates to KILL after a deadline. It rejects an absent host bus mapping. The example unit was not installed.
- Gate: passed for implementation and recorded validation scope.


### Task 11: Run integrated regression and live validation

Files: focused test files/modules, private validation evidence, this plan's acceptance matrix.

Todo:

- [x] Run the full CPU test suite and Clippy through Fedora.
- [ ] Check formatting and build a release review binary in a separate target directory.
- [ ] Verify the supported CUDA configuration compiles when its runtime is available. Otherwise mark it untested with the reason.
- [x] Re-run existing capture integrity, final delivery, history, no-speech, cancellation, and recent-audio tests.
- [x] Run the complete controller and menu concurrency scenarios from Tasks 3 and 8.
- [ ] Verify the same binary's icons and microphone submenu on Plasma, Waybar, and Quickshell.
- [ ] Test high DPI, panel icon hiding, menu opening, active selection, and panel restart.
- [x] Verify behavior with no watcher, late watcher startup, watcher restart, and bus loss.
- [ ] Record a short private dictation from each physical input and the supported virtual source.
- [ ] Select an input, unplug it, attempt start, reconnect it, refresh, and start again.
- [ ] Test stream failure, server loss, suspend/resume, differing input formats, and busy selection.
- [x] Verify final words and stop-tail draining with controlled audio or a repeatable capture fixture.
- [ ] Verify automatic delivery and clipboard fallback in a disposable focused client, using an isolated review daemon.
- [x] Confirm status and the tray update while finalization or portal authorization is held.
- [ ] Compare startup time, idle CPU, process memory, dependency count, and release binary size with Task 0/1 baselines.
- [ ] Record introduced overhead and investigate unexplained regressions. Do not invent numerical claims without measurements.
- [x] Keep live audio and transcripts private. Save only non-sensitive summarized evidence in repository documentation.

Do: distinguish a synthetic barrier test from a live desktop or microphone result.

Don't: mark an unavailable environment passed. Record it as untested and leave its release gate open.

Validation: all automated checks pass relative to the recorded baseline. Required live target rows have evidence. Any release exception is explicit and narrows the stated tested support.

Review focus: realistic failure scenarios, test isolation, meaningful assertions, live evidence, and existing behavior regressions. This task requires independent review when available.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [ ] Gate passed: automated checks and required live targets have recorded results.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: src/app_tray_tests.rs; src/tray_dbus_tests.rs; all changed source.
- Checks: The full suite passed 164 tests with five ignored. Two opt-in tests passed separately. Clippy passed with warnings denied. Formatting and the CPU release build passed.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: Seven release hypotheses matched baseline output. Seven full daemon replays matched saved history and original audio. Saved history is existing ASR output, not corrected ground truth. Broad live validation remains partial.
- Gate: partial; remaining live rows are explicitly untested.


### Task 12: Complete documentation and final review

Files: `README.md`, help text, deployment documentation, `THIRD_PARTY_NOTICES.md`, this plan. Update domain glossary or ADRs only when their content actually changes.

Todo:

- [x] Document icon meanings, input selection, refresh, missing-device behavior, and runtime-only preferences.
- [x] Document tray-disable behavior and notification fallback.
- [x] State tested desktop/panel targets separately from protocol-compatible targets.
- [x] Document Distrobox bus requirements without implying every container uses the same socket path.
- [x] Document build dependencies, runtime requirements, and CPU/CUDA verification status.
- [x] Verify command examples, links, and actual CLI output.
- [x] Review the full feature diff against the implementation base, including cross-task interactions.
- [x] Confirm every task review record and acceptance row is complete or explicitly unresolved.
- [x] Record remaining optional follow-ups: persisted preference, hotplug subscriptions, live switching, and richer feedback.
- [x] Prepare a concise change description with behavior, checks, and material limitations.
- [x] Document rollback before later deployment. Keep a known working binary and identify the matching launch command.
- [x] Explain that `--no-tray` disables presentation only. It does not revert the changed audio backend or controller implementation.
- [x] Keep release deployment separate from implementation completion. Do not replace or restart the active daemon implicitly.

Do: describe the final implementation, not abandoned alternatives or unsupported compatibility claims.

Don't: close a gate based on a passing unrelated test or an old review before later edits.

Validation: documentation matches the final binary and final feature diff. No blocking review findings remain.

Review focus: scope completion, wording accuracy, deployment limits, dependency notices, and consistency across tasks.

- [x] Code-quality review recorded.
- [x] Behavior review recorded.
- [x] Gate passed: the feature is reviewable and ready for a separately authorized deployment.

Implementation record, 2026-10-03:

- Base: `68fa5285929de590a1793c2df909f06798a760fb`. Final change is the uncommitted working tree.
- Files: README.md; this plan; full feature diff.
- Checks: Independent standards and behavior reviews covered Tasks 0 through 12. Reported blockers were corrected and affected checks reran. Documentation separates verified behavior from protocol compatibility.
- Standards reviewer: `tray_standards_review`. Findings and corrections are recorded below.
- Behavior reviewer: `tray_spec_review`. Findings and corrections are recorded below.
- Evidence and limits: The known previous binary and production daemon remain in place. No service, panel, or system input default was changed. Final release deployment is separate from this implementation.
- Gate: passed for implementation and recorded validation scope.


## Validation command patterns

Run these from the implementation checkout. The target directory must differ from the active daemon's target directory.

```bash
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo fmt --all -- --check
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo test --locked \
  --target-dir target/tray-review
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo clippy --locked \
  --all-targets --target-dir target/tray-review -- -D warnings
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo build --locked --release \
  --bin nvstt --target-dir target/tray-review
rtk proxy distrobox enter --name Fedora -- mise exec -- cargo tree --locked -e features
```

Use focused test filters within the task loop. Run the complete checks after shared-interface changes and at Task 11. Do not run `--all-features` blindly: CPU and CUDA builds have distinct native runtime arrangements.

For test daemons, set task-specific XDG config, state, data, and runtime locations. Retain access to a valid Wayland and audio session through the explicit launch arrangement. A temporary runtime directory may require explicit display/audio socket addresses; changing it alone can disconnect those protocols. Do not copy the production portal restore token or overwrite production history.

## Acceptance matrix

Complete the evidence column during implementation. `Pending` is not a passing result.

| Requirement | Primary tasks | Evidence / result |
| --- | --- | --- |
| Five truthful icons and startup feedback | 2, 7, 9 | Passed pure icon tests; Plasma ready/listening screenshots. Other phases use tested projection. |
| Failure survives transition back to idle | 2, 9 | Passed retained delivery-error and clear-error tests. |
| Clipboard fallback is visibly distinct from automatic delivery | 2, 9, 11 | Passed outcome and notification policy tests; live clipboard delivery not rerun. |
| Runtime selection applies to the next dictation | 4, 5, 6, 8 | Passed input lifecycle tests and three real source start/cancel checks. |
| Busy and stale selection requests cannot apply later | 3, 5, 8 | Passed controller barriers and real stale D-Bus event tests. |
| Missing device does not trigger silent substitution | 4, 5, 8 | Passed deterministic pinned-device loss and recovery test; physical unplug untested. |
| Physical and virtual inputs are available; monitors are labeled | 1, 4, 8 | Passed live five-entry catalog and monitor metadata checks. |
| Tray and status remain responsive during slow work | 3, 7, 11 | Passed finalization/delivery barriers; live incomplete-client status took 0.85 ms. |
| No partial or duplicate delivery | 3, 5, 11 | Passed existing final-only and capture-integrity tests. |
| Existing history and recent-audio contracts remain valid | 5, 11 | Passed full regression suite and seven exact original-audio daemon replays. |
| One item works on Plasma | 7, 8, 10, 11 | Passed registration, ready/listening screenshot inspection, and D-Bus menu checks. |
| Same item works on Waybar | 7, 8, 10, 11 | Untested live. Standard SNI/DBusMenu protocol is the compatibility target. |
| Same item works on a Quickshell tray/menu configuration | 7, 8, 10, 11 | Untested live. Standard SNI/DBusMenu protocol is the compatibility target. |
| Host absence/restart leaves dictation usable | 7, 9, 11 | Passed absent, late, and restarted watcher tests on a private bus. |
| Correct host session from Fedora deployment | 10, 11 | Passed explicit host-bus mapping and supervised container-child shutdown. |
| Existing CLI and IPC behavior remains compatible | 3, 6, 11 | Passed existing tests, legacy status parsing, additive requests, and CLI help checks. |
| Clean controller/tray/socket shutdown | 3, 7, 11 | Passed controller shutdown barriers, isolated daemon stop/socket cleanup, and launcher TERM/KILL probes. |
| No transcript or portal token in tray content | 2, 7, 11 | Passed private-text projection test; no transcript or token enters menu labels. |
| Runtime-only preference and deployment limits documented | 6, 10, 12 | Documented in README and task records. |
| Every task reviewed and findings resolved | 0 through 12 | Two independent review axes recorded. Broad live validation remains partial. |

## Progress and decisions

- [x] Task 0 complete.
- [x] Task 1 complete.
- [x] Task 2 complete.
- [x] Task 3 complete.
- [x] Task 4 complete.
- [x] Task 5 complete.
- [x] Task 6 complete.
- [x] Task 7 complete.
- [x] Task 8 complete.
- [x] Task 9 complete.
- [x] Task 10 complete.
- [ ] Task 11 complete. Automated and available live checks passed. Broader live validation remains partial.
- [x] Task 12 complete.

Record implementation decisions and gate results here or directly under the relevant task. Include date, reason, affected tasks, and evidence. Do not erase an unresolved limitation when updating status.

## Planning verification

The plan was checked against the researched scope and the current source interfaces. All 13 tasks have todos, dos, don'ts, validation, review focus, and both review gates. Local document links and Markdown structure were checked. This was a self-review of the plan. No implementation, build, live capture, or desktop test ran during plan creation.

## Final implementation decisions and review findings

The application uses standard SNI and DBusMenu. The historical `org.kde` interface name does not introduce a KDE library dependency.

The tray contains one flat microphone radio group. Monitors carry explicit labels. This keeps one selected preference across hosts.

History reads reject busy operation admission instead of waiting behind delivery. Status reads remain independent. A controller reservation permits one accepted operation at a time.

Failure labels show safe stage hints. CLI status provides full backend reasons. This adjusts the earlier short-reason requirement to keep raw private text out of the tray.

Icons use cached Rust-generated shapes instead of separate image files. Their data uses network-order ARGB at three sizes.

Standards review found stale item-ID reuse, completion ordering, orphaned container children, and a wrong-bus fallback. The implementation corrected each finding. Behavior review also found stale input labels, lost monitor classification, duplicate labels, early menu re-enabling, and missing warnings. Those findings are corrected.

The final CPU release artifact was 57,238,776 bytes. The preserved production artifact was 52,070,696 bytes. These are different builds, so their difference is not a controlled overhead measurement. The lockfile grew from 371 to 378 packages. Idle CPU, memory, and startup comparisons remain unmeasured.

Private artifacts remain under `~/.local/state/nvstt-tray-verification-2nirboqh`. They include snapshots, baseline/final evaluations, daemon replay reports, screenshots, and launcher shutdown evidence. None are repository files.

Untested live cases remain Waybar, Quickshell, high DPI, hidden-item behavior, physical unplug/reconnect, capture server loss, suspend/resume, full session-bus replacement, and CUDA. The existing external typing and clipboard paths were not rerun against a focused application. Deterministic delivery and clipboard tests passed.

The production daemon stayed active. Test daemons used private sockets and stores. Tests did not change the system input default. Optional later work includes saved preference, hotplug subscriptions, live switching, and richer feedback.
