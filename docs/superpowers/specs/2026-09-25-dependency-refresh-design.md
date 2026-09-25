# Dependency refresh and focused API cleanup

Status: scope approved; user delegated execution while away.

## Purpose

Bring this Rust app's direct dependencies to their latest stable, non-yanked releases and refresh the lockfile's compatible transitive packages. Use newer APIs only when they remove code or complexity without changing user-visible behavior. Build and link a verified release binary for the user to test. Do not restart the running daemon.

The user chose a focused upgrade, not a broad code reduction or architecture rewrite. They also asked for better output text and delegated decisions while away. Use measured, behavior-preserving improvements here; do not claim accuracy gains or expand into a model or gate-policy change without enough corrected evidence. Fewer lines are useful only when the code remains clear and its behavior stays correct.

## Scope and selection

- Work on `feature/recent-dictation-audio` in its existing linked worktree. Do not merge or push. Leave the main checkout's untracked files and the worktree's existing untracked research notes untouched.
- Inventory every direct normal, development, and build dependency in `Cargo.toml`. Check each against its current stable, non-yanked release, including major versions. Update explicit constraints and regenerate `Cargo.lock` so the resolver picks the newest compatible transitive versions. Do not add speculative dependencies or enable unrelated default features.
- Keep Rust 1.98.1 through `mise exec --`. Check Linux build support, required system libraries, crate features, and minimum Rust versions before selecting each release. Preserve the app's CPU-default and optional CUDA runtime choice. Test the CPU build on this host; record any CUDA verification that this host cannot perform.
- Treat `sherpa-onnx` and `sherpa-onnx-sys` as one native-runtime upgrade. Check the effective native archive version and any local library override, not only the Rust package version. The upstream 1.13.5 NeMo greedy-decoder fix makes a newer release worth testing. It does not prove that either corrected phrase improves.
- If a latest stable release cannot build or pass required tests without changing the approved product behavior, stop and report the incompatible dependency and choices. Do not silently keep an older release while claiming that every dependency is current.

## Implementation approach

First record a fresh baseline. Update dependencies in small groups so build or test failures have a clear source: native recognition, audio capture and processing, then CLI, delivery, storage, and support crates. Resolve the lockfile after each group. Do not bundle unrelated refactors with version bumps.

Read upstream migration guidance and relevant changed APIs for each group. Replace a local workaround or adapter only if the new API performs the same job with less complexity. Add or adjust tests before any intended behavior change. Do not remove capture-integrity checks, private-file handling, or final-only delivery to reduce line count. Keep a short review record of accepted simplifications, rejected opportunities, and compatibility limits.

Preserve the selected Nemotron 560 ms model artifact, speech-gate default and parameters, denoise and text-processing settings, seven retained private audio entries, ten text-history records, CLI, IPC, and delivery behavior. Keep audio out of the microphone callback's disk path. Do not introduce a Python daemon, alternative model, new configuration key, or transcript upload.

## Verification and release

Run `mise exec -- cargo test --quiet` before changes. After each dependency group, run its focused tests and address new warnings or failures at the affected call site. At the end, run the full tests, `mise exec -- cargo fmt --all -- --check`, `mise exec -- cargo clippy --all-targets -- -D warnings`, and `git diff --check`. Report ignored tests and any unsupported feature build instead of calling them passing.

Use an isolated Cargo target directory for the candidate release. The existing `~/.local/bin/nvstt` link already points into this worktree, so rebuilding its current target would expose an unverified binary. Build the candidate with `mise exec -- cargo build --release --locked` and `CARGO_TARGET_DIR` set to a separate ignored directory under the worktree's `target/`. Keep the current linked binary unchanged until all checks pass.

Before the first dependency change, take a private, bounded snapshot of every currently retained, store-owned WAV and its metadata outside the repository. Use owner-only permissions, reject symlinks, and record checksums so the same files can be replayed after archive rotation. Replay that full recording set with the baseline binary before the upgrade and with the candidate binary afterward, using the same model export and profile with gate on and off. Include saved no-speech and failed attempts in the inventory; report which cannot produce a transcript. Compare transcripts to text history only as uncorrected historical output, never as accuracy ground truth. Check whether the two corrected phrases supplied by the user appear, and report other changed word spans, applicable latency, and memory limits. These two phrases are targeted checks, not a word-error-rate result or authority to change the default gate. Keep WAVs, metadata, corrected references, and detailed output reports in private local storage only; never commit or upload them. Trash the temporary snapshot after successful comparison. If a file was already absent before the snapshot, report that limit.

Only after verification, atomically replace the symlink at `~/.local/bin/nvstt` with one to the tested release binary. Confirm the link resolves to that exact executable. Do not install a service, stop or restart a daemon, or claim a running process adopted the new code. Tell the user that the existing daemon keeps its prior executable until a separate restart.

## Acceptance criteria

1. Each direct dependency matches its checked current stable release, or the user receives a named blocker before any partial release is linked. The lockfile is internally consistent and up to date within dependency constraints.
2. The focused API review records actual code simplifications and rejected changes. Every code change serves a version migration or a clearly beneficial API replacement.
3. The project's required default-feature tests, formatting, Clippy, and locked release build pass. The result preserves CLI, IPC, capture integrity, private audio retention, and final-only delivery.
4. Private before-and-after replay uses the same snapshot of all available recent WAVs, reports what changed in the two corrected phrases, and does not claim overall recognition improvement. No personal audio or corrected reference enters Git or a remote service.
5. `~/.local/bin/nvstt` resolves to the verified release binary. The running daemon remains untouched. User-owned untracked files remain unchanged.
