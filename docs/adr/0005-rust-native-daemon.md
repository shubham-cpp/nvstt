# Build a native Rust daemon and CLI

The first implementation uses Rust for both the long-running voice daemon and
its CLI client. Rust gives one native Linux binary, strong async and IPC
support, direct access to Wayland and D-Bus boundaries, and a usable
`sherpa-onnx` Parakeet integration. Python remains a development and benchmark
tool, not a required runtime.

## Consequences

- The delivery layer may need a small C FFI boundary for libei until bindings
  mature.
- The model archive remains separate from the executable.
- Audio, recognition, delivery, history, and notifications can be tested as
  replaceable Rust interfaces.
