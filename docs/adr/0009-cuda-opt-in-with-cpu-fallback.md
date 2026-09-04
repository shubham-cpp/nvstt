# Keep CUDA opt-in with CPU fallback

Status: accepted

nvstt offers CUDA as an opt-in inference provider. CUDA runs in a separate
companion binary. The portable CPU binary remains the fallback.

This phase does not switch from CUDA to CPU inside one process. A failed native
CUDA provider can abort in foreign code. A future automatic fallback must use a
separate preflight process.

CUDA ships as an optional companion build. The portable CPU build remains the
default installation.

## Consequences

- The CUDA runtime is a separate, version-pinned dependency.
- The CUDA build and the CPU build are packaged separately.
- Status reports the provider selected by the active binary.
- CPU remains the default provider.
