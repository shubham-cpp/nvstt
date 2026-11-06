#!/usr/bin/env python3
"""Freeze private audio, run repeatable native evaluations, and compare results."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def snapshot(args):
    entries = []
    for metadata in args.recordings.glob("*/metadata.json"):
        value = json.loads(metadata.read_text())
        entries.append((value["stopped_at_ms"], metadata.parent))
    selected = sorted(entries)[-args.limit:]
    if not selected:
        raise ValueError("No saved recordings found")
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    rows = []
    for index, (_, directory) in enumerate(selected, 1):
        clip = args.output / f"clip-{index:02d}"
        shutil.copytree(directory, clip)
        audio = clip / "audio.wav"
        rows.append({"id": clip.name, "audio": str(audio.relative_to(args.output)),
                     "audio_sha256": digest(audio), "category": "dictation",
                     "reference": None, "raw_reference": None,
                     "required_phrases": []})
    manifest = args.output / "manifest.jsonl"
    manifest.write_text("".join(json.dumps(row) + "\n" for row in rows))
    print(manifest)
    print("References are unverified. Add reviewed references or phrase expectations before gating.")


def corpus(manifest):
    rows = [json.loads(line) for line in manifest.read_text().splitlines() if line.strip()]
    if not rows or len({row["id"] for row in rows}) != len(rows):
        raise ValueError("Corpus must contain unique clip IDs")
    for row in rows:
        audio = manifest.parent / row["audio"]
        actual = digest(audio)
        expected = row.get("audio_sha256")
        if expected and expected != actual:
            raise ValueError(f"Audio changed for {row['id']}")
        row["audio_sha256"] = actual
        row.pop("audio")  # Location does not affect corpus identity.
    return hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest()


def run(args):
    identity = corpus(args.manifest)
    binary = args.binary.resolve()
    binary_identity = digest(binary)
    status_command = [str(binary), "model", "status", "--model", args.model, "--json"]
    if args.profile:
        status_command += ["--streaming-profile", args.profile]
    status_result = subprocess.run(status_command, capture_output=True, text=True, timeout=30, check=True)
    status = json.loads(status_result.stdout)
    model_files = {entry["name"]: Path(entry["path"]) for entry in status["files"]}
    if status["speech_gate"]["enabled"]:
        model_files["speech_gate"] = Path(status["speech_gate"]["path"])
    model_hashes = {name: digest(path) for name, path in model_files.items()}
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    reports = []
    failures = []
    for index in range(args.repeats):
        command = [str(binary), "model", "evaluate", "--manifest", str(args.manifest.resolve()),
                   "--json", "--check", "--model", args.model, "--threads", str(args.threads)]
        if args.profile:
            command += ["--streaming-profile", args.profile]
        if args.max_wer is not None:
            command += ["--max-word-error-rate", str(args.max_wer)]
        if args.max_finalization_ms is not None:
            command += ["--max-finalization-ms", str(args.max_finalization_ms)]
        result = subprocess.run(command, capture_output=True, text=True, timeout=args.timeout)
        (args.output / f"run-{index + 1}.stderr").write_text(result.stderr)
        if not result.stdout.strip():
            raise ValueError(f"Evaluation returned no report; inspect {args.output}")
        report = json.loads(result.stdout)
        write_json(args.output / f"run-{index + 1}.json", report)
        reports.append(report)
        if result.returncode:
            failures.append(index + 1)
    if corpus(args.manifest) != identity or digest(binary) != binary_identity:
        raise ValueError("Audio, references, or binary changed during evaluation")
    if model_hashes != {name: digest(path) for name, path in model_files.items()}:
        raise ValueError("Model files changed during evaluation")
    summary = {"schema_version": 1, "corpus_sha256": identity, "binary_sha256": binary_identity,
               "model_sha256": model_hashes,
               "platform": platform.platform(), "config": reports[0]["config"],
               "repeats": args.repeats, "failed_runs": failures,
               "word_error_rate": max((r["word_error_rate"] for r in reports if r["word_error_rate"] is not None), default=None),
               "scored_clips": reports[-1]["scored_clips"], "clips": len(reports[-1]["clips"]),
               "silent_clip_failures": max(r["silent_clip_failures"] for r in reports),
               "expectation_failures": max(r["expectation_failures"] for r in reports),
               "median_p95_finalization_ms": statistics.median(r["p95_finalization_latency_ms"] for r in reports),
               "worst_p95_finalization_ms": max(r["p95_finalization_latency_ms"] for r in reports),
               "median_real_time_factor": statistics.median(r["real_time_factor"] for r in reports),
               "unstable_clips": [reports[0]["clips"][i]["id"] for i in range(len(reports[0]["clips"]))
                                  if len({r["clips"][i]["hypothesis"] for r in reports}) > 1]}
    write_json(args.output / "summary.json", summary)
    print(json.dumps(summary, indent=2))
    return bool(failures or summary["unstable_clips"])


def compare(args):
    before, after = (json.loads(p.read_text()) for p in (args.baseline, args.candidate))
    if before["corpus_sha256"] != after["corpus_sha256"]:
        raise ValueError("Corpora differ; compare identical audio and references")
    failures = []
    for name in ("silent_clip_failures", "expectation_failures"):
        if after[name] > before[name]:
            failures.append(f"{name} increased")
    if after["failed_runs"] or after["unstable_clips"]:
        failures.append("candidate has failed or unstable runs")
    if before["word_error_rate"] is not None and after["word_error_rate"] is not None:
        if after["word_error_rate"] > before["word_error_rate"] + args.wer_tolerance:
            failures.append("word error rate increased")
    elif args.require_references:
        failures.append("reviewed references are missing")
    if args.require_references and after["scored_clips"] != after["clips"]:
        failures.append("not every clip has a reviewed reference")
    if after["median_p95_finalization_ms"] > before["median_p95_finalization_ms"] * args.latency_ratio:
        failures.append("finalization latency increased beyond budget")
    print(json.dumps({"passed": not failures, "failures": failures}, indent=2))
    return bool(failures)


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    snap = commands.add_parser("snapshot")
    snap.add_argument("--recordings", type=Path, default=Path.home() / ".local/state/nvstt/recordings")
    snap.add_argument("--output", type=Path, required=True)
    snap.add_argument("--limit", type=int, choices=range(1, 8), default=7)
    runner = commands.add_parser("run")
    runner.add_argument("--manifest", type=Path, required=True)
    runner.add_argument("--output", type=Path, required=True)
    runner.add_argument("--binary", type=Path, default=Path("target/release/nvstt"))
    runner.add_argument("--model", default="parakeet-unified-en-0.6b")
    runner.add_argument("--profile")
    runner.add_argument("--threads", type=int, choices=range(1, 33), default=4)
    runner.add_argument("--repeats", type=int, choices=range(1, 11), default=3)
    runner.add_argument("--timeout", type=float, default=600)
    runner.add_argument("--max-wer", type=float)
    runner.add_argument("--max-finalization-ms", type=int, default=2000)
    comparison = commands.add_parser("compare")
    comparison.add_argument("baseline", type=Path)
    comparison.add_argument("candidate", type=Path)
    comparison.add_argument("--wer-tolerance", type=float, default=0.0)
    comparison.add_argument("--latency-ratio", type=float, default=1.2)
    comparison.add_argument("--require-references", action="store_true")
    args = parser.parse_args()
    for name in ("max_wer", "wer_tolerance", "latency_ratio", "timeout"):
        value = getattr(args, name, None)
        if value is not None and (not math.isfinite(value) or value < 0):
            parser.error(f"{name} must be finite and nonnegative")
    try:
        return {"snapshot": snapshot, "run": run, "compare": compare}[args.command](args) or 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"Reliability check failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
