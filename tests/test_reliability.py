"""Run with python3 -m unittest discover -s tests -p 'test_*.py'."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location(
    "reliability", Path(__file__).parents[1] / "scripts/reliability.py"
)
reliability = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reliability)


class ReliabilityTests(unittest.TestCase):
    def test_changed_audio_cannot_reuse_a_baseline_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio = root / "audio.wav"
            audio.write_bytes(b"original")
            manifest = root / "manifest.jsonl"
            manifest.write_text(json.dumps({"id": "one", "audio": "audio.wav",
                "audio_sha256": hashlib.sha256(b"original").hexdigest()}))
            reliability.corpus(manifest)
            audio.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "Audio changed"):
                reliability.corpus(manifest)

    def test_comparison_rejects_new_omissions_and_missing_references(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = {"corpus_sha256": "same", "silent_clip_failures": 0,
                "expectation_failures": 0, "failed_runs": [], "unstable_clips": [],
                "word_error_rate": None, "scored_clips": 0, "clips": 7,
                "median_p95_finalization_ms": 500}
            before, after = root / "before.json", root / "after.json"
            before.write_text(json.dumps(baseline))
            args = SimpleNamespace(baseline=before, candidate=after, wer_tolerance=0,
                                   latency_ratio=1.2, require_references=False)
            with contextlib.redirect_stdout(io.StringIO()):
                after.write_text(json.dumps(baseline))
                self.assertFalse(reliability.compare(args))
                after.write_text(json.dumps({**baseline, "expectation_failures": 1}))
                self.assertTrue(reliability.compare(args))
                after.write_text(json.dumps(baseline))
                args.require_references = True
                self.assertTrue(reliability.compare(args))

    def test_snapshot_does_not_invent_reference_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            recording = root / "recordings" / "one"
            recording.mkdir(parents=True)
            (recording / "metadata.json").write_text('{"stopped_at_ms":1}')
            (recording / "audio.wav").write_bytes(b"audio")
            args = SimpleNamespace(recordings=root / "recordings", output=root / "corpus", limit=7)
            with contextlib.redirect_stdout(io.StringIO()):
                reliability.snapshot(args)
            row = json.loads((args.output / "manifest.jsonl").read_text())
            self.assertIsNone(row["reference"])
            self.assertEqual(row["audio_sha256"], hashlib.sha256(b"audio").hexdigest())


if __name__ == "__main__":
    unittest.main()
