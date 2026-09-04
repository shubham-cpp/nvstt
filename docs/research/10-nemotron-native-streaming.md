# Nemotron native streaming research

Date: 2026-08-03

## Decision

Use `nemotron-speech-streaming-en-0.6b` with the official `560ms` INT8
Sherpa-ONNX artifact. Put local Silero VAD before the recognizer.

## Model comparison

| Model | Streaming method | Fit for nvstt |
| --- | --- | --- |
| Parakeet Unified 0.6B | Buffered streaming. It reuses a window. | Keep as a rollback model. |
| Nemotron streaming 0.6B | Cache-aware FastConformer-RNNT streaming. It keeps context between frames. | Use as the candidate model. |
| Whisper | Offline or rolling-window decoding. | Do not use for this final-only, low-latency path. |

NVIDIA describes Nemotron as a streaming FastConformer-RNNT model. Its stream
state lets each new frame use prior context without independent ASR jobs.
Sherpa-ONNX loads the encoder, decoder, joiner, and token files through its
online transducer API.

The pinned artifact is:

```text
sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25
```

## Why independent five-second chunks are out of scope

Five-second chunks discard model context at every boundary. They repeat model
work and can lose or duplicate words near the boundary. A rolling overlap
needs text merge rules. It also makes final release process a full last chunk.

Nemotron already provides the needed streaming boundary. It processes new
audio once and retains context. Release only flushes its tail. The daemon
still delivers one final transcript after the hotkey release.

## Speech gate

The local Silero VAD uses 16 kHz, 512-sample frames, threshold 0.5, 250 ms
minimum speech, 500 ms minimum silence, 400 ms pre-roll, and a 30-second
bounded detector buffer. It does not finish a dictation on silence. It only
stops silence from entering the recognizer.

A no-speech dictation succeeds without delivery or history. Cancellation
resets both the VAD and ASR stream.

## Evaluation gate

Use a private JSONL WAV manifest. Compare the current Parakeet 1120 ms setup
with Nemotron 560 ms. Include silence or noise, short dictation, 10–30 second
dictation, proper nouns, and known failures.

Do not change the local active configuration until Nemotron has fewer total
word errors, zero silent deliveries, no lost final word, and warmed p95
release-to-delivery latency of one second or less. The CUDA companion must
report `cuda` and must not be slower than CPU.

## Implementation smoke result

The pinned artifact loaded through both native CPU and CUDA binaries. CUDA
reported `cuda`. The Silero gate suppressed a bundled alarm WAV without text.

This small vendor corpus is not the private release corpus. It gave three
word errors for Parakeet 1120 ms and nine for Nemotron 560 ms with the speech
gate. Nemotron without the gate gave four. Do not use this result to switch
the active configuration. It shows why the private corpus controls release.

## Sources

- [NVIDIA Nemotron model card](https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b)
- [Sherpa-ONNX online ASR models](https://k2-fsa.github.io/sherpa/onnx/c-api/html/online_asr.html)
- [Sherpa-ONNX Rust VAD API](https://docs.rs/sherpa-onnx/latest/sherpa_onnx/struct.VoiceActivityDetector.html)
- [NVIDIA NeMo streaming ASR guidance](https://docs.nvidia.com/nemo/speech/nightly/asr/inference.html)
