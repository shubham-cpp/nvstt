# Parakeet finalization investigation

Date: 2026-08-03

## Test method

The benchmark uses five seconds of the bundled model sample. It feeds 20 ms
audio chunks without sleeping. It has one warm-up run. It measures a virtual
real-time backlog and the actual model finalization time. The backlog is an
estimate. It does not include the worker command polling delay.

The RTX 3050 uses the CUDA companion. The CPU test uses the release CPU
binary. These runs do not include text delivery.

## Results

| Provider | Profile | Runs | P95 finalization | Backlog | Result |
| --- | --- | ---: | ---: | ---: | --- |
| CPU | 560 ms | 5 | 3.532 s | 2.523 s | Fail |
| CUDA | 560 ms | 5 | 2.181 s | 1.346 s | Fail |
| CUDA | 240 ms | 1 | 7.648 s | 7.062 s | Fail |
| CPU | 1120 ms | 20 | 0.542 s | 0 s | Pass |
| CUDA | 1120 ms | 20 | 0.459 s | 0 s | Pass |

The earlier user recording gave a 1.967 s CPU P95 for the 560 ms profile.
This supports the same result: the default profile misses the one-second goal.

## Findings

The 560 ms profile cannot process each stream before more audio arrives. It
leaves queued audio at stop. This backlog causes the long finalization delay.

CUDA works on the RTX 3050. The 560 ms profile becomes faster, but it still
leaves more than one second of queued audio. CUDA alone does not meet the goal.

The 240 ms profile calls the model too often on this hardware. It makes the
backlog much worse.

The 1120 ms profile processes five seconds of audio in about 1.5 to 1.9
seconds. It leaves no queue at stop. CUDA reduces P95 by about 83 ms from the
CPU result.

Model load takes 2.5 to 5.6 seconds. The daemon keeps the model warm. Model
load does not affect normal dictation finalization.

## Decision still needed

The 1120 ms profile meets the latency goal. It can change transcript accuracy.
Keep 560 ms as the default until an accuracy test approves 1120 ms.

## References

- [Parakeet Unified GPU runtime](https://k2-fsa.github.io/sherpa/onnx/install/linux.html)
- [ONNX Runtime execution providers](https://onnxruntime.ai/docs/execution-providers/)
- [NVIDIA CUDA driver compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/minor-version-compatibility.html)
