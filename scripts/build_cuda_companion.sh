#!/usr/bin/env bash
# Build a CUDA-enabled nvstt companion binary against a local runtime.

set -euo pipefail

readonly DATA_ROOT="${XDG_DATA_HOME:-"${HOME:?HOME must be set}/.local/share"}"
readonly DEFAULT_RUNTIME="${DATA_ROOT}/nvstt/cuda-runtime-12.6.3-cudnn-9.3.0.75"
readonly RUNTIME_DIR="${1:-$DEFAULT_RUNTIME}"
readonly SHERPA_LIB_DIR="${RUNTIME_DIR}/sherpa"
readonly CUDA_LIB_DIR="${RUNTIME_DIR}/cuda"
readonly CUDA_TARGET_DIR="target/nvstt-cuda-build"

if [[ ! -f "${SHERPA_LIB_DIR}/libsherpa-onnx-c-api.so" ]]; then
    printf 'missing sherpa runtime: %s\n' "$SHERPA_LIB_DIR" >&2
    exit 1
fi
if [[ ! -f "${CUDA_LIB_DIR}/libcudnn.so.9" ]]; then
    printf 'missing CUDA runtime: %s\n' "$CUDA_LIB_DIR" >&2
    exit 1
fi

CCACHE_DISABLE=1 SHERPA_ONNX_LIB_DIR="$SHERPA_LIB_DIR" cargo build --release \
    --bin nvstt --example transcribe_wav --no-default-features --features cuda-runtime \
    --target-dir "$CUDA_TARGET_DIR"

printf 'CUDA app: %s/release/nvstt\n' "$CUDA_TARGET_DIR"
printf 'CUDA benchmark: %s/release/examples/transcribe_wav\n' "$CUDA_TARGET_DIR"
printf 'Run either command with LD_LIBRARY_PATH=%s\n' "$CUDA_LIB_DIR"
