# CUDA companion runtime

This app has two native builds.

- The default build uses the CPU. It remains portable.
- The `cuda-runtime` build uses CUDA. It is an opt-in companion build.

The companion needs CUDA 12 libraries and cuDNN 9. The NVIDIA driver alone
does not contain all these libraries.

The install script downloads these pinned user-space libraries:

- CUDA 12.6.3 components: cudart, cuBLAS, cuFFT, and cuRAND.
- cuDNN 9.3.0.75 for CUDA 12.
- sherpa-onnx 1.13.4 built for CUDA 12 and cuDNN 9.

It verifies each downloaded SHA-256 checksum. It does not use `sudo`. It does
not change the Bazzite image or the installed NVIDIA driver.

Run the installer from the repository root:

```bash
./scripts/install_cuda_runtime.sh
```

Then build the CUDA companion:

```bash
./scripts/build_cuda_companion.sh
```

The script prints the exact runtime directory. Set it before you start the CUDA
benchmark:

```bash
runtime_dir="${XDG_DATA_HOME:-$HOME/.local/share}/nvstt/cuda-runtime-12.6.3-cudnn-9.3.0.75"
LD_LIBRARY_PATH="$runtime_dir/cuda${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  target/nvstt-cuda-build/release/examples/transcribe_wav \
  --model-dir "${XDG_DATA_HOME:-$HOME/.local/share}/nvstt/models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms" \
  --benchmark --iterations 20 /tmp/nvstt-latency-2.wav
```

The host NVIDIA driver resolves `libcuda.so`. The local runtime resolves the
other CUDA libraries. NVIDIA documents that newer drivers run applications
built with older CUDA toolkits. Your R610 driver supports the CUDA 12 runtime.

If the CUDA companion does not load, keep the default CPU binary in use. It
does not share the CPU binary's native runtime.

## Compare Nemotron on CPU and CUDA

Install Nemotron once without changing the active configuration:

```bash
nvstt model install --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms
```

Run the same private manifest with the CPU binary and the CUDA companion. The
JSON `execution_provider` field must report `cuda` for the companion. Keep the
CPU binary if CUDA cannot load the model or is slower.

```bash
LD_LIBRARY_PATH="$runtime_dir/cuda${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  target/nvstt-cuda-build/release/nvstt model evaluate \
  --manifest /private/corpus/manifest.jsonl \
  --model nemotron-speech-streaming-en-0.6b --streaming-profile 560ms --json
```

Do not switch the daemon configuration until the corpus passes the accuracy,
silence, final-word, and warmed p95 latency gates.

## Test the lower-latency profile

Use these Parakeet profiles only for rollback testing. A profile can change
accuracy and stream processing cost.

```bash
./scripts/install_parakeet_streaming_profile.sh 240ms
./scripts/install_parakeet_streaming_profile.sh 1120ms
```

Pass the installed model directory to `transcribe_wav` when you benchmark it.
Keep the 560 ms profile as the default until its accuracy test passes.

To select an installed profile for the daemon, set this in
`~/.config/nvstt/config.toml`:

```toml
model = "parakeet-unified-en-0.6b"
streaming_profile = "1120ms"
```

Run `nvstt model install` after you change the profile. The command installs
the selected artifact if it is not already present.

Run `nvstt status --json` after the daemon starts. The `execution_provider`
field reports `cpu` for the default binary or `cuda` for the companion binary.
