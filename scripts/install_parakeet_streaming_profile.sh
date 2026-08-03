#!/usr/bin/env bash
# Install a measured Parakeet Unified streaming profile next to the default.

set -euo pipefail

readonly PROFILE="${1:-}"
case "$PROFILE" in
    240ms)
        readonly ARTIFACT="sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-240ms"
        readonly ARCHIVE_SIZE="501358456"
        readonly ARCHIVE_SHA256="dead05a9149f6f02d373f3eb4553c74af4f189ff74889b5e210b35ca102655da"
        ;;
    1120ms)
        readonly ARTIFACT="sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-1120ms"
        readonly ARCHIVE_SIZE="501356335"
        readonly ARCHIVE_SHA256="4788229a6dd03be33f8243ccee48e33a8d15df7b448cb99150b0ccddd1b02d74"
        ;;
    *)
        printf '%s\n' "usage: $0 PROFILE" >&2
        printf '%s\n' "profiles: 240ms, 1120ms" >&2
        exit 2
        ;;
esac

readonly DATA_ROOT="${XDG_DATA_HOME:-"${HOME:?HOME must be set}/.local/share"}"
readonly MODEL_ROOT="${DATA_ROOT}/nvstt/models"
readonly TARGET="${MODEL_ROOT}/${ARTIFACT}"
readonly CACHE_ROOT="${XDG_CACHE_HOME:-"${HOME:?HOME must be set}/.cache"}/nvstt"
readonly ARCHIVE="${CACHE_ROOT}/${ARTIFACT}.tar.bz2"
readonly URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/${ARTIFACT}.tar.bz2"

if [[ -e "$TARGET" ]]; then
    printf 'model already exists: %s\n' "$TARGET" >&2
    exit 1
fi

mkdir -p "$MODEL_ROOT" "$CACHE_ROOT"
readonly STAGING="$(mktemp -d "${MODEL_ROOT}/.${ARTIFACT}.install.XXXXXX")"

for attempt in {1..40}; do
    if [[ -f "$ARCHIVE" ]] && (( $(stat --format=%s "$ARCHIVE") > ARCHIVE_SIZE )); then
        printf 'trimming excess bytes from %s\n' "$ARTIFACT"
        truncate --size "$ARCHIVE_SIZE" "$ARCHIVE"
    fi

    if printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE" | sha256sum --check --status 2>/dev/null; then
        break
    fi

    if [[ -f "$ARCHIVE" ]] && (( $(stat --format=%s "$ARCHIVE") == ARCHIVE_SIZE )); then
        printf 'restarting a corrupt %s download\n' "$ARTIFACT"
        rm -f -- "$ARCHIVE"
    fi

    printf 'downloading %s (attempt %s)\n' "$ARTIFACT" "$attempt"
    curl --continue-at - --fail --location --retry 3 --silent --show-error \
        --output "$ARCHIVE" "$URL"
done

if ! printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE" | sha256sum --check --status; then
    printf 'checksum did not match: %s\n' "$ARTIFACT" >&2
    exit 1
fi

tar --extract --bzip2 --file "$ARCHIVE" --directory "$STAGING"
for file in encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt; do
    if [[ ! -f "$STAGING/$ARTIFACT/$file" ]]; then
        printf 'model archive did not contain: %s\n' "$file" >&2
        exit 1
    fi
done

mv "$STAGING/$ARTIFACT" "$TARGET"
rmdir "$STAGING"
rm -f -- "$ARCHIVE"

printf 'installed model: %s\n' "$TARGET"
