# Local hardware and Parakeet model fit

Research date: 2026-08-02.

This report records the hardware visible from the Codex desktop sandbox and
maps it to practical local runtimes for NVIDIA Parakeet.  The sandbox is a
`bwrap` container.  It can see PCI and kernel-driver information, but it does
not expose the host's `/dev/nvidia*`, `/dev/dri`, desktop session, or PipeWire
socket.  GPU and microphone performance must therefore be verified once from a
host process.

## Initial recommendation

Use the quantized sherpa-onnx Parakeet Unified streaming model as the first
production backend:

```text
sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms
```

Load one long-lived online recognizer when the daemon starts. Feed 16 kHz mono
chunks while the CLI state is `listening`; on the second toggle, finalize the
stream and inject only the final text. This path does not type partial text.
The model archive is about 640 MB and current sherpa-onnx provides
`OnlineRecognizer` bindings and CPU Linux binaries.

Keep CUDA as an optional execution provider.  The machine has an NVIDIA
GeForce RTX 3050 6GB Laptop GPU, so the 0.6B model should fit in VRAM, but the
desktop daemon must not require the NVIDIA device or CUDA libraries.  Probe
the provider at startup and fall back to CPU when the host or a packaged
environment cannot open `/dev/nvidia0`.

Use NeMo/PyTorch or Hugging Face Transformers only for model parity checks and
benchmarking.  They are much heavier to install and ship than the ONNX
runtime.  `parakeet.cpp`/GGUF is a useful later alternative when a fully
standalone C++/ggml backend is more important than sherpa-onnx's existing
Rust and microphone/VAD integration.

## Observed machine

The following values came from read-only files and commands on 2026-08-02.

| Resource | Observation | Relevance |
| --- | --- | --- |
| Kernel | `Linux bazzite 7.1.3-ogc5.1.fc44.x86_64`, built 2026-07-21 | x86_64 Linux; recent enough for current ONNX and CUDA runtimes |
| OS image | Fedora Linux 44 (container image), hostname `bazzite` | Bazzite/Fedora desktop packaging should be treated as immutable; do not assume system-wide package writes |
| CPU | AMD Ryzen 7 7840HS with Radeon 780M Graphics; 8 cores / 16 threads; boost enabled; max 5137.9038 MHz; AVX2 and AVX-512 feature flags visible | Strong CPU fallback.  Benchmark with 2, 4, 8, and 16 inference threads rather than reserving all cores |
| Memory | `MemTotal: 15,614,416 kB` (about 14.9 GiB); `MemAvailable` at inspection: 5.35 GiB | INT8 Parakeet Unified should fit comfortably; avoid loading multiple F32/PyTorch models while the desktop is active |
| Swap | 18,874,364 kB total (about 18 GiB); 7.2 GiB free at inspection | Swap is a safety net, not an acceptable hot-path working set |
| Root storage | 475 GB filesystem, 299 GB free (37% used) | Enough for model archives and a bundled runtime; store models under an XDG data directory |
| Discrete GPU | `/proc/driver/nvidia/gpus/0000:01:00.0/information`: `NVIDIA GeForce RTX 3050 6GB Laptop GPU`, PCI `10de:25ac`, PCIe, Video BIOS `94.07.88.00.75` | 6 GB VRAM class is sufficient for a 0.6B model, subject to precision and activation memory |
| GPU driver | NVIDIA Open Kernel Module `610.43.03` | Driver is visible, but this sandbox has no NVIDIA device nodes; host CUDA probing is still required |
| Integrated GPU | PCI `1002:15bf`, `amdgpu`, Radeon 780M; sysfs reports 512 MiB visible VRAM | Useful for display, not a target for the CUDA backend |
| Audio codecs | ALSA cards: NVIDIA HDMI, AMD HDMI (`Generic`), AMD/Realtek (`Generic_1`, codec `Realtek ALC245`), and AMD `acp63` DMIC capture | Internal analog/DMIC capture paths exist; PipeWire should select the default source in the real user session |
| Audio formats | ALC245 analog capture exposes 44.1/48/96/192 kHz, 16/20/24-bit; sherpa resamples to 16 kHz | Capture through PipeWire or ALSA and convert to mono 16-bit/16 kHz before inference |
| User-space runtime | Python `3.14.6`, Rust `1.97.1`, Cargo `1.97.1`; no `pip`, `uv`, `arecord`, `pactl`, or `pw-record` executable was visible in the sandbox | Do not make the system Python or command-line audio tools runtime dependencies |
| CUDA user libraries | `libcuda.so.1` and NVIDIA 610.43.03 libraries are visible; no CUDA toolkit/CuDNN packages were installed in the container | CPU must be the install-free baseline; CUDA needs a host/package preflight |

The GPU identity is from NVIDIA's own open-kernel-module device table and the
kernel proc entry, not from a guessed laptop SKU: NVIDIA lists PCI device
`25AC` as “GeForce RTX 3050 6GB Laptop GPU”.

## Parakeet variants and runtime choices

| Option | Artifact and operating mode | Fit on this machine | Recommendation |
| --- | --- | --- | --- |
| sherpa-onnx Parakeet Unified INT8 streaming | Buffered RNNT streaming encoder/decoder/joiner; approximately 624 MB + 6.9 MB + 1.7 MB plus tokens. The 560 ms profile is one of several latency exports. | Fits the available RAM. The CPU provider is supported; benchmark 160–2080 ms profiles and 2/4/8 threads. | **Production default** |
| NVIDIA NeMo Parakeet Unified | `nvidia/parakeet-unified-en-0.6b`, 600M parameters, reference NeMo 2.7.3 runtime, buffered streaming with 160 ms minimum latency. | Useful for parity tests, but PyTorch/NeMo packaging is heavier and GPU support is the reference path. | Development and parity benchmark |
| sherpa-onnx TDT v3 INT8 | Multilingual 25-language offline model with streaming options, approximately 640 MB. | Fits the machine and remains a strong multilingual fallback. | Future multilingual profile |
| parakeet.cpp GGUF | C++/ggml implementation with quantized model variants. | Attractive for a standalone CPU binary; streaming integration is less established for this project. | Evaluate later |

Parakeet Unified is English-only. It emits punctuation and capitalization and
uses chunked self-attention with buffered streaming. TDT v3 remains the
multilingual option for Bulgarian, Croatian, Czech, Danish, Dutch, English,
Estonian, Finnish, French, German, Greek, Hungarian, Italian, Latvian,
Lithuanian, Maltese, Polish, Portuguese, Romanian, Slovak, Slovenian, Spanish,
Swedish, Russian, and Ukrainian.

### Why INT8 ONNX is the right first fit

The ONNX archive keeps the large encoder near 624 MB instead of the roughly
2.4 GB F32 parameter payload. It avoids the startup and packaging cost of
PyTorch, can run without an Internet connection after the model download, and
uses sherpa-onnx's `OnlineRecognizer` buffered RNNT path. Keep the recognizer
loaded across toggles so model load time is paid once.

The sherpa streaming export example uses `num_threads=3` and a 560 ms profile.
Its published RTF is a maintainer-machine reference, not a benchmark of this
laptop. The 7840HS has 8 physical cores, so test 2/4/8 threads and 160/240/560
ms profiles before selecting the smallest setting that preserves accuracy and
desktop responsiveness.

### CUDA caveat

sherpa-onnx documents pre-built Linux CUDA wheels and binaries.  The CUDA 11.8
path needs CUDA 11.8; the newer path needs CUDA 12.x with cuDNN 9.  The current
wheel index contains CPython 3.14 Linux x86_64 CUDA builds, but exact package
versions and CUDA sonames must be pinned and checked at release time.  A
working NVIDIA kernel module alone is not enough: the process also needs
`/dev/nvidia0` (and usually `/dev/nvidia-uvm`) and compatible CUDA/cuDNN shared
libraries.  If any probe fails, use the CPU provider and keep the same model.

## Packaging implications for Fedora/Bazzite

1. Prefer a native Rust daemon with the `sherpa-onnx` Rust crate.  The current
   crate wraps the public C API with RAII types and supports both
   `OfflineRecognizer` and `OnlineRecognizer`.  Its default build links a
   matching prebuilt library archive; an offline release build must vendor or
   cache that archive rather than download during installation.
2. Ship or download the model separately under
   `$XDG_DATA_HOME/<app>/models/`.  Verify an archive checksum before use and
   expose a `model status` command.  Do not place 640 MB of model data in every
   small source package when a first-run download is acceptable.
3. Keep a CPU-only build path.  Fedora/Bazzite updates can change NVIDIA or
   CUDA library versions, and an immutable base image may not permit users to
   install a toolkit.  An optional CUDA artifact can be selected after a
   provider preflight.
4. Capture audio through the user's PipeWire session (or an ALSA fallback),
   not by spawning `arecord`.  The Codex sandbox has no session bus, but the
   normal desktop service will.  Convert input to the model's required mono,
   16 kHz PCM format.

## Benchmark plan before locking defaults

Run these tests on the host, outside the Codex sandbox, after the daemon can
load a model:

1. Download the official Parakeet Unified INT8 streaming archive and its
   included test WAVs. Record model load time separately from warm inference.
2. Feed the same 5 s, 15 s, and 60 s clips through the 160/240/560 ms profiles
   with CPU `num_threads=1,2,4,8,16`. Record real-time factor, p50/p95 final
   latency, peak RSS, interim stability, and CPU temperature/power if available.
   Pinning the process to 4-8 cores can improve desktop responsiveness.
3. If `nvidia-smi` works, run the same clips with `provider=cuda`.  Record VRAM
   peak, first-run CUDA kernel/JIT cost, warm latency, and behavior after
   suspend/resume.  Do not treat a CUDA run as successful if it silently falls
   back to CPU.
4. Capture real microphone samples from the ALC245 analog input, the DMIC, and
   one headset.  Test silence, keyboard noise, short phrases, punctuation,
   numbers, and a 30-60 s paragraph.  Compare text and latency for CPU and
   CUDA.
5. Use an initial acceptance target of warm RTF <= 0.30 (at least 3x real time),
   <= 1.5 s from stop-toggle to text for a 10 s utterance, and <= 4 GiB peak
   RSS for the daemon.  Adjust these targets after observing real user
   expectations; they are product targets, not upstream guarantees.

## Sources

- [NVIDIA Parakeet Unified model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b) — 600M parameters, English-only unified offline/streaming architecture, latency profiles, and NeMo runtime.
- [sherpa-onnx streaming export](https://github.com/k2-fsa/sherpa-onnx/pull/3602) — INT8 streaming archive, 560 ms profile, `OnlineRecognizer`, and CPU example.
- [sherpa-onnx changelog](https://github.com/k2-fsa/sherpa-onnx/blob/master/CHANGELOG.md) — buffered RNNT streaming path for Parakeet Unified.
- [NVIDIA Parakeet TDT v3 model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) — multilingual future profile.
- [sherpa-onnx Parakeet/NeMo model list](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-transducer/nemo-transducer-models.html) — v3 and v2 model conversion, archive component sizes, CPU/GPU examples, and reference RTF.
- [sherpa-onnx installation](https://k2-fsa.github.io/sherpa/onnx/python/install.html) and [Linux CUDA build notes](https://k2-fsa.github.io/sherpa/onnx/install/linux.html) — CPU wheels, CUDA 11.8/12.x + cuDNN requirements, and build options.
- [sherpa-onnx API overview](https://k2-fsa.github.io/sherpa/onnx/index.html) — offline/streaming ASR and supported language bindings.
- [sherpa-onnx Rust crate](https://docs.rs/sherpa-onnx/latest/sherpa_onnx/) — `OfflineRecognizer`, `OnlineRecognizer`, static prebuilt-library behavior, and `SHERPA_ONNX_LIB_DIR` override.
- [parakeet.cpp](https://github.com/mudler/parakeet.cpp) — GGUF quantization choices, v3 support, and NeMo parity claim.
- [NVIDIA open GPU kernel modules device table](https://github.com/NVIDIA/open-gpu-kernel-modules/blob/main/README.md) — PCI `25AC` is listed as GeForce RTX 3050 6GB Laptop GPU.
