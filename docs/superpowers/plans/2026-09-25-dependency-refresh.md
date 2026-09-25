# Dependency Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade every direct Rust dependency to the checked latest stable release, refresh compatible transitive dependencies, and link a tested release binary.

**Architecture:** Preserve the existing daemon and CLI interfaces. Upgrade native recognition separately from other crates, then make only necessary or demonstrably simpler API changes. Compare the same private saved WAVs before and after; leave the running daemon alone.

**Tech Stack:** Rust 2024, Cargo 1.98.1 through mise, sherpa-onnx, CPAL, ureq, TOML, private local WAV replay.

**Spec:** `docs/superpowers/specs/2026-09-25-dependency-refresh-design.md`

## Global Constraints

- Work only in the linked `feature/recent-dictation-audio` worktree. Do not merge or push.
- Keep `main`'s untracked files and the worktree's existing untracked research notes untouched.
- Stable non-yanked direct releases checked on 2026-09-25: `bzip2 0.6.1`, `clap 4.6.7`, `cpal 0.18.2`, `sherpa-onnx 1.13.8`, `thiserror 2.0.21`, `toml 0.9.8`, `ureq 3.4.2`. The other 14 direct packages already resolve to their latest stable release. Recheck at execution time.
- Preserve Nemotron 560 ms, speech-gate default, privacy, original audio, seven WAVs, ten text records, final-only delivery, CLI and IPC. No Python rewrite or model switch.
- Use `mise exec -- cargo ...`; preserve default CPU and optional CUDA feature definitions. Record any CUDA verification this host cannot run.
- No personal audio or corrected transcript goes into Git, logs sent remotely, or any external ASR service.
- Stop and report an incompatible latest release rather than linking a partly updated build as if all dependencies were current.
- Do not restart or install the daemon. Build away from the current symlink target, then atomically link the verified candidate.

## Review Focus

1. Ureq 3 must keep model download redirects, HTTPS, connect/body timeouts, content-length checks, and partial-download failures. Add a local loopback HTTP test for a truncated response and one for successful download.
2. Native Sherpa updates may change words, silence handling, or stream finalization. Replay the identical private WAV set with gate on and off before and after; never use text history as corrected truth.
3. Bzip2 0.6 must not weaken archive-path escape checks or model-file validation. Keep the existing archive extraction tests green.
4. TOML 0.9 must preserve invalid-config errors and nested settings. Keep configuration and replacement tests green.
5. CPAL 0.18.2 must preserve dropped-sample and backend failure reporting. Keep recorder and daemon capture-integrity tests green; no hardware claim without a live test.

---

### Task 1: Private baseline and release isolation

**Files:** Private snapshot and reports under `~/.local/state/nvstt/` only. No product code changes.

**Interfaces:** Consumes `src/recordings.rs` owned-entry metadata and `model evaluate --manifest ... --speech-gate ... --json`. Produces a private snapshot path, checksums, and two baseline JSON reports for later tasks.

- [ ] **Step 1: Confirm the worktree and current link.** Run `git status --short`, `readlink ~/.local/bin/nvstt`, and `mise exec -- cargo test --quiet`. Report failures before changing dependencies.
- [ ] **Step 2: Snapshot the currently retained store-owned WAVs.** After `umask 077`, create `snapshot="$(mktemp -d "$HOME/.local/state/nvstt/dependency-refresh.XXXXXXXX")"`, then `export SNAPSHOT="$snapshot"`. Run the following Python. It rejects symlinks and foreign entries, copies the WAV and metadata with private modes, and writes checksums and an uncorrected manifest. Do not mutate the seven-entry store.

```python
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat

state = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state") / "nvstt"
root = state / "recordings"
snapshot = Path(os.environ["SNAPSHOT"])
assert root.is_dir() and not root.is_symlink()
assert snapshot.is_dir() and snapshot.stat().st_mode & 0o777 == 0o700
history = {row["id"]: row for row in json.loads((state / "history.json").read_text())}
manifest = []
checksums = {}

def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()

for folder in root.iterdir():
    if folder.is_symlink() or not folder.is_dir():
        continue
    sources = [folder / "metadata.json", folder / "audio.wav"]
    if not all(stat.S_ISREG(source.lstat().st_mode) for source in sources if source.exists()):
        continue
    if not all(source.is_file() and not source.is_symlink() for source in sources):
        continue
    meta = json.loads(sources[0].read_text())
    if meta.get("version") != 1 or folder.name != f"{meta['stopped_at_ms']:020d}-{meta['session_id']}":
        continue
    dest = snapshot / folder.name
    dest.mkdir(mode=0o700)
    for source in sources:
        target = dest / source.name
        with os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW), "rb") as inp:
            with os.fdopen(os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as out:
                shutil.copyfileobj(inp, out)
        assert digest(source) == digest(target)
        checksums[str(target.relative_to(snapshot))] = digest(target)
    manifest.append({"id": meta["session_id"], "audio": str((dest / "audio.wav").resolve()),
                     "category": "dictation", "reference": history.get(meta["session_id"], {}).get(
                         "transcript", "uncorrected diagnostic placeholder")})
assert manifest, "no retained WAVs available"
(snapshot / "manifest.jsonl").write_text("".join(json.dumps(row) + "\n" for row in manifest))
(snapshot / "sha256.json").write_text(json.dumps(checksums, indent=2))
print("private_snapshot_entries=", len(manifest), "path=", snapshot)
```

- [ ] **Step 3: Check inventory.** Record the number of copied WAVs and their sample rates, model/profile, gate setting, and transcription/capture statuses. If a retained file used a different model or profile, do not silently evaluate it as Nemotron 560 ms; report it or group it under its installed original model. Treat every manifest reference as uncorrected history, not accuracy ground truth.
- [ ] **Step 4: Replay both settings before updates.** In an owner-only shell (`umask 077`), run the current linked binary twice:

```bash
"$HOME/.local/bin/nvstt" model evaluate --manifest "$snapshot/manifest.jsonl" --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --speech-gate true --json > "$snapshot/before-on.json"
"$HOME/.local/bin/nvstt" model evaluate --manifest "$snapshot/manifest.jsonl" --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --speech-gate false --json > "$snapshot/before-off.json"
```

- [ ] **Step 5: Inspect statuses, not false WER.** Compare snapshot entry count, checksum list, recognition errors, and presence of the two confirmed phrases. Do not quote an aggregate WER from uncorrected history. Record the before-state privately.

### Task 2: Upgrade the native recognition runtime

**Files:** Modify `Cargo.toml`, `Cargo.lock`; test via existing `src/recognizer.rs`, `src/speech_gate.rs`, `src/evaluation.rs` tests. Change Rust source only if 1.13.8 requires it.

**Interfaces:** Consumes the exact saved model artifact and snapshot from Task 1. Produces a compile-ready `sherpa-onnx` and `sherpa-onnx-sys` 1.13.8 pair without changing recognizer settings.

- [ ] **Step 1: Confirm published version and native origin.** Check the official crate index, release notes for v1.13.5 through v1.13.8, and the `sherpa-onnx-sys` build's native archive or library overrides. Preserve `cpu-runtime` and `cuda-runtime` features.
- [ ] **Step 2: Change the one direct constraint.** Replace `sherpa-onnx = { version = "1.13.4", default-features = false }` with `version = "1.13.8"`. Use `mise exec -- cargo update -p sherpa-onnx -p sherpa-onnx-sys`; check that both locked versions are 1.13.8. If Cargo rejects the command due to an ambiguous package selector, use `-p sherpa-onnx@1.13.4 -p sherpa-onnx-sys@1.13.4`.
- [ ] **Step 3: Verify existing contract tests.** Run `mise exec -- cargo test recognizer:: --quiet`, `mise exec -- cargo test speech_gate:: --quiet`, and `mise exec -- cargo test evaluation:: --quiet`. If a test fails, isolate the exact native behavior before editing app code. Add a failing test before any intended behavior change.
- [ ] **Step 4: Review API deltas.** Compare the crate's published methods with `OnlineTransducerRecognizer` and `VadGatedRecognizer`. Remove an adapter only if existing tests prove equivalent behavior. Do not change the gate/model defaults to make an output look better.

### Task 3: Refresh other crates and migrate the installer

**Files:** Modify `Cargo.toml`, `Cargo.lock`, and only affected code in `src/installer.rs`, `src/config.rs`, `src/recorder.rs`, `src/error.rs`. Test the existing installer, config, recorder, and app modules; add installer loopback tests if needed.

**Interfaces:** Keeps `install_model`, `Config::load`, `CpalRecorder`, and `AppError` behavior. Produces current direct dependencies plus latest resolver-compatible lockfile packages.

- [ ] **Step 1: Migrate download tests first.** In `src/installer.rs` tests, add a loopback `TcpListener` fixture that serves a small complete response and a Content-Length larger than the bytes sent. Assert `download_file` reports byte count/progress for the complete response and a truncated-download error for the short one. The fixture must never contact public endpoints.
- [ ] **Step 2: Update manifest versions.** Replace the direct requirements for `bzip2` with `0.6.1` (retain `static`), `clap` with `4.6.7`, `cpal` with `0.18.2`, `thiserror` with `2.0.21`, `toml` with `0.9.8`, and `ureq` with `3.4.2`. Preserve existing feature choices unless the new package requires a documented equivalent.
- [ ] **Step 3: Use current HTTP APIs.** In `download_file`, replace `ureq::AgentBuilder` with `ureq::Agent::config_builder()`. Use `.timeout_connect(Some(Duration::from_secs(30)))`, `.timeout_recv_body(Some(Duration::from_secs(30)))`, `.user_agent(concat!("nvstt/", env!("CARGO_PKG_VERSION")))`, `.build().new_agent()`. Read `content-length` from `response.headers().get("content-length").and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok())`. Use `response.body_mut().as_reader()` for the streaming copy. Keep the existing `create_new`, length validation, progress, and sync order.
- [ ] **Step 4: Run focused tests.** Run `mise exec -- cargo test installer:: --quiet`, `mise exec -- cargo test config:: --quiet`, `mise exec -- cargo test recorder:: --quiet`, and `mise exec -- cargo test app:: --quiet`. Fix only migration breakage; add a failing test first if a new API changes behavior.
- [ ] **Step 5: Refresh resolver-compatible transitive packages.** Run `mise exec -- cargo update`, then inspect `Cargo.lock` for direct-version mismatches and incompatible Rust versions. Recheck the official latest stable direct versions. If a current stable direct release cannot work without a policy change, stop and ask rather than claim full completion.

### Task 4: API-led simplification, full checks, and link

**Files:** Review the actual diff in `Cargo.toml`, `Cargo.lock`, and any touched `src/*.rs`. Write only a public, non-private summary if needed. Link only the verified binary outside Git.

**Interfaces:** Consumes Tasks 1-3 results. Produces one tested release binary and a checked symlink; does not change a running daemon.

- [ ] **Step 1: Review changed APIs and code.** Use official migration notes to identify any obsolete compatibility code. Keep a simplification only when it shortens or clarifies an existing module without loosening an invariant. Reject unrelated refactors explicitly.
- [ ] **Step 2: Run final checks.** Run `mise exec -- cargo fmt --all -- --check`, `mise exec -- cargo clippy --all-targets -- -D warnings`, `mise exec -- cargo test --quiet`, and `git diff --check`. Record the CUDA verification limit if the host lacks its native runtime.
- [ ] **Step 3: Build without replacing the live link target.** From the worktree, run:

```bash
export CARGO_TARGET_DIR="$PWD/target/dependency-refresh"
mise exec -- cargo build --release --locked
test -x "$CARGO_TARGET_DIR/release/nvstt"
```

- [ ] **Step 4: Replay the identical snapshot with the candidate.** With `umask 077`, run the candidate `model evaluate` commands from Task 1 into private `after-on.json` and `after-off.json`. Verify saved WAV checksums are unchanged; compare target phrases, all other word-span changes, failed/no-speech entries, and report limits. Do not use uncorrected text as accuracy ground truth.
- [ ] **Step 5: Review independently.** Ask a fresh reviewer to inspect the full diff for regressions and accidental API/behavior changes; fix verified issues and repeat affected tests.
- [ ] **Step 6: Atomically update the link only after success.** Verify `~/.local/bin/nvstt` is a symlink, create a temporary owner-only directory in `~/.local/bin`, create a symlink inside it to `$CARGO_TARGET_DIR/release/nvstt`, and use `mv -T` to replace the existing symlink. Check `readlink` and `realpath` resolve to the tested binary. Trash the empty temporary directory with `gio trash`; do not restart the daemon.
- [ ] **Step 7: Close the private session.** Trash the temporary audio snapshot using `gio trash "$snapshot"` only after reporting results. Confirm no private audio, corrected phrases, or reports entered `git status` or commits. State that the running daemon still uses its previous executable until restarted.
