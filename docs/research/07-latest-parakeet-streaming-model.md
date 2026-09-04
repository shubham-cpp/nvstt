# Latest Parakeet 0.6B streaming model

Research date: 2026-08-02.

## Result

The current NVIDIA Parakeet 0.6B model intended for streaming is
`nvidia/parakeet-unified-en-0.6b`, released on 2026-04-07. It is not the older
TDT v3 model. It is English-only and uses a 600M-parameter Unified FastConformer
encoder with an RNN-T decoder.

NVIDIA documents one shared checkpoint for offline and buffered streaming
inference. The model supports configurable chunk plus right-context latency
from 2.08 seconds down to 160 ms. NVIDIA notes that 160 ms loses some accuracy
from limited right context, so the initial profile should use 560 ms and be
benchmarked before lowering latency.

The first release should stream audio into the recognizer while the user is in
the `listening` state. It should not type partial text. On the stop toggle, the
daemon finalizes the stream, obtains the final transcript, and then runs the
existing native-first delivery policy.

## Runtime choice

NeMo 2.7.3 is NVIDIA's reference runtime. Current sherpa-onnx releases export
buffered RNNT streaming variants, including an INT8
`parakeet-unified-en-0.6b-int8-streaming-560ms` archive. The sherpa export uses
`OnlineRecognizer` and a CPU provider is available. Pin the sherpa-onnx version
and archive together; older sherpa documentation lists only the non-streaming
export.

## Trade-off against TDT v3

TDT v3 remains the better choice for 25 European languages and offline batch
transcription. It is not the requested latest 0.6B streaming-first model. Keep
it as a future profile rather than silently substituting it.

## Sources

- [NVIDIA Parakeet Unified model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b) — model identity, language, architecture, latency range, and streaming configuration.
- [NVIDIA NeMo Speech model selection](https://docs.nvidia.com/nemo/speech/nightly/starthere/choosing_a_model.html) — streaming model guidance and comparison with TDT v3.
- [NVIDIA NeMo Speech repository](https://github.com/NVIDIA-NeMo/NeMo) — release note for Parakeet Unified and NeMo runtime context.
- [sherpa-onnx streaming export PR](https://github.com/k2-fsa/sherpa-onnx/pull/3602) — INT8 streaming archives, 560 ms profile, `OnlineRecognizer`, and CPU example.
- [sherpa-onnx changelog](https://github.com/k2-fsa/sherpa-onnx/blob/master/CHANGELOG.md) — buffered RNNT streaming path for Parakeet Unified.
