# Use the latest Parakeet Unified 0.6B streaming model

Use NVIDIA's `parakeet-unified-en-0.6b` as the first recognizer. It is a 600M
parameter English FastConformer-RNNT model trained for both offline and
buffered streaming inference. The Rust daemon feeds audio to a long-lived
stream during `listening`, but delivers text only after the stop toggle. Pin an
INT8 sherpa-onnx streaming export with the 560 ms profile as the accuracy
baseline. Test lower-latency profiles only when a measured timing problem
remains on CUDA. For a warm model, 95% of five-second dictations must produce
their final transcript within one second of the stop toggle.

## Consequences

- The model is English-only. Multilingual Parakeet TDT v3 remains a future
  model profile, not the default.
- The recognizer boundary must support `OnlineRecognizer` stream lifecycle,
  endpointing, finalization, and reset between toggles.
- Streaming inference reduces stop-to-transcript latency, but partial text is
  not delivered to the focused client in the first release.
- The model archive and the sherpa-onnx runtime must be version-pinned because
  streaming exports and latency profiles are runtime-specific.
