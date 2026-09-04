# Supplied sample verification

Verification date: 2026-08-02.

The MP3 in `dist/` was decoded to 16 kHz mono, then passed to
`examples/transcribe_wav.rs` with the extracted
`sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms` archive.
The run completed on CPU with the same `ParakeetRecognizer` used by the
daemon.

The first 30 seconds matched the opening of the supplied reference text. The
full 3 minute 47 second recording recovered the spoken content. The output
contains normal ASR substitutions, including `Sahil` rendered as `Sahi` and
`Jai Bharat` rendered as `Jai Baharad`. Music markers and some non-spoken
reference annotations were not expected from an ASR model.

The model file set was checked before the run:

- `encoder.int8.onnx`
- `decoder.int8.onnx`
- `joiner.int8.onnx`
- `tokens.txt`

No model archive or decoded audio is committed to the repository. Re-run the
check with:

```bash
cargo run --example transcribe_wav -- \
  --model-dir "$model_root/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms" \
  sample.wav
```
