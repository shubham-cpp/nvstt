#!/usr/bin/env bash
# Install the NVIDIA libraries for the nvstt CUDA companion build.
#
# This does not change the Bazzite image or NVIDIA driver. It creates a
# versioned, user-local runtime under XDG_DATA_HOME instead.

set -euo pipefail

readonly CUDA_RELEASE="12.6.3"
readonly CUDNN_RELEASE="9.3.0.75"
readonly SHERPA_RELEASE="1.13.4"
readonly DATA_ROOT="${XDG_DATA_HOME:-"${HOME:?HOME must be set}/.local/share"}"
readonly DEFAULT_DESTINATION="${DATA_ROOT}/nvstt/cuda-runtime-${CUDA_RELEASE}-cudnn-${CUDNN_RELEASE}"

if [[ "${1:-}" == "--help" ]]; then
    printf '%s\n' "usage: $0 [DESTINATION]"
    printf '%s\n' ""
    printf '%s\n' "Install the pinned CUDA 12 and cuDNN 9 libraries without sudo."
    exit 0
fi

readonly DESTINATION="${1:-$DEFAULT_DESTINATION}"
readonly DESTINATION_PARENT="$(dirname "$DESTINATION")"
readonly CACHE_ROOT="${XDG_CACHE_HOME:-"${HOME:?HOME must be set}/.cache"}/nvstt/cuda-runtime-${CUDA_RELEASE}-cudnn-${CUDNN_RELEASE}"

if [[ -e "$DESTINATION" ]]; then
    printf 'runtime already exists: %s\n' "$DESTINATION" >&2
    printf '%s\n' "Remove it only if you intend to reinstall this exact version." >&2
    exit 1
fi

mkdir -p "$DESTINATION_PARENT"
mkdir -p "$CACHE_ROOT"
readonly STAGING_DIR="$(mktemp -d "${DESTINATION_PARENT}/.nvstt-cuda-install.XXXXXX")"
mkdir -p "$STAGING_DIR/cuda" "$STAGING_DIR/sherpa"

download_with_resume() {
    local name="$1"
    local url="$2"
    local expected_sha256="$3"
    local expected_size="$4"
    local archive="$5"

    for attempt in {1..40}; do
        if [[ -f "$archive" ]] && (( $(stat --format=%s "$archive") > expected_size )); then
            printf 'trimming excess bytes from %s\n' "$name"
            truncate --size "$expected_size" "$archive"
        fi

        if printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status 2>/dev/null; then
            return 0
        fi

        if [[ -f "$archive" ]] && (( $(stat --format=%s "$archive") == expected_size )); then
            printf 'restarting a corrupt %s download\n' "$name"
            rm -f -- "$archive"
        fi

        printf 'downloading %s (attempt %s)\n' "$name" "$attempt"
        curl --continue-at - --fail --location --retry 3 --silent --show-error \
            --output "$archive" "$url"
    done

    printf 'checksum did not match after 40 download attempts: %s\n' "$name" >&2
    return 1
}

download_and_extract_xz() {
    local name="$1"
    local url="$2"
    local expected_sha256="$3"
    local expected_size="$4"
    local archive="$CACHE_ROOT/${name}.tar.xz"

    download_with_resume "$name" "$url" "$expected_sha256" "$expected_size" "$archive"
    tar --extract --xz --file "$archive" --directory "$STAGING_DIR/cuda" \
        --strip-components=2 --wildcards --no-anchored --exclude='*/lib/stubs/*' 'lib/*.so*'
}

download_and_extract_sherpa() {
    local archive="$CACHE_ROOT/sherpa-onnx.tar.bz2"
    local url="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${SHERPA_RELEASE}/sherpa-onnx-v${SHERPA_RELEASE}-cuda-12.x-cudnn-9.x-linux-x64-gpu.tar.bz2"
    local expected_sha256="2ba80dd4df761b8de58d578190846f6f2349523685e33bcb24f65ba586c43563"
    local expected_size="201655924"

    download_with_resume "sherpa-onnx ${SHERPA_RELEASE} CUDA runtime" "$url" "$expected_sha256" "$expected_size" "$archive"
    tar --extract --bzip2 --file "$archive" --directory "$STAGING_DIR/sherpa" \
        --strip-components=2 --wildcards --no-anchored 'lib/*.so*'
}

download_and_extract_xz \
    "cuda-cudart" \
    "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-12.6.77-archive.tar.xz" \
    "f74689258a60fd9c5bdfa7679458527a55e22442691ba678dcfaeffbf4391ef9" \
    "1126072"
download_and_extract_xz \
    "cublas" \
    "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/linux-x86_64/libcublas-linux-x86_64-12.6.4.1-archive.tar.xz" \
    "ec682bac6387f9cdfd0c20b25a16cd6ed0b8b3b7ff42be9eaeb41828e3a72572" \
    "522827284"
download_and_extract_xz \
    "cufft" \
    "https://developer.download.nvidia.com/compute/cuda/redist/libcufft/linux-x86_64/libcufft-linux-x86_64-11.3.0.4-archive.tar.xz" \
    "63a046d51a45388e10612c3fd423bb7fa5127496aa9bb3951a609e8b9d996852" \
    "476376920"
download_and_extract_xz \
    "curand" \
    "https://developer.download.nvidia.com/compute/cuda/redist/libcurand/linux-x86_64/libcurand-linux-x86_64-10.3.7.77-archive.tar.xz" \
    "981339cc86d7b8779e9a3c17e72d8c5e1a8a2d06c24db692eecabed8e746a3c7" \
    "81729748"
download_and_extract_xz \
    "cudnn" \
    "https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/linux-x86_64/cudnn-linux-x86_64-9.3.0.75_cuda12-archive.tar.xz" \
    "3d6ef10aa06dc9339a477e2b057e085ff8500bbdee79e42c7e13655c9eff2c26" \
    "756509380"
download_and_extract_sherpa

for library in \
    "$STAGING_DIR/cuda/libcudart.so.12" \
    "$STAGING_DIR/cuda/libcublas.so.12" \
    "$STAGING_DIR/cuda/libcublasLt.so.12" \
    "$STAGING_DIR/cuda/libcufft.so.11" \
    "$STAGING_DIR/cuda/libcurand.so.10" \
    "$STAGING_DIR/cuda/libcudnn.so.9" \
    "$STAGING_DIR/sherpa/libsherpa-onnx-c-api.so" \
    "$STAGING_DIR/sherpa/libonnxruntime_providers_cuda.so"; do
    if [[ ! -e "$library" ]]; then
        printf 'required library was not extracted: %s\n' "$library" >&2
        exit 1
    fi
done

mv "$STAGING_DIR" "$DESTINATION"
rm -f -- \
    "$CACHE_ROOT/cuda-cudart.tar.xz" \
    "$CACHE_ROOT/cublas.tar.xz" \
    "$CACHE_ROOT/cufft.tar.xz" \
    "$CACHE_ROOT/curand.tar.xz" \
    "$CACHE_ROOT/cudnn.tar.xz" \
    "$CACHE_ROOT/sherpa-onnx.tar.bz2"
rmdir "$CACHE_ROOT" 2>/dev/null || true

printf 'installed CUDA runtime: %s\n' "$DESTINATION"
printf '%s\n' "Next: scripts/build_cuda_companion.sh $DESTINATION"
