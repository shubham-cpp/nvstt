# Use Nemotron native streaming with a speech gate

Status: accepted

Use Nemotron streaming 0.6B with the 560 ms INT8 Sherpa-ONNX artifact for new
installs. Use one native online transducer stream for each dictation. Add a
local Silero speech gate before it.

Do not use independent five-second ASR chunks. Nemotron keeps streaming state
between input frames. The VAD gate prevents silence and noise from creating a
transcript. It does not end a dictation on silence.

Parakeet Unified remains in the closed model registry. An existing Parakeet
configuration does not gain VAD unless the user enables it. Rollback is a
configuration change and daemon restart.

## Consequences

- The ASR model and VAD file install as one atomic model directory.
- A no-speech result does not deliver text or add history.
- The CPU and CUDA builds stay separate. There is no in-process GPU fallback.
- Private-corpus evaluation controls activation of the local Nemotron setting.
