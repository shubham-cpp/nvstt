# Capture boundaries at toggle

Date: 2026-10-04

## Reproduction

The user still reported missing first and last words after the decoder repairs.
An isolated PulseAudio monitor reproduced a 2.045-second microphone-stream startup delay with the server's default buffers.
The test used the real CPAL recorder and audio server. It did not open a physical microphone.
The initial callback contained audio before the recorder returned, but startup took too long for immediate speech.

Normal stop destroyed the stream immediately. It had no allowance for device buffering or the final spoken sound.
The new tail-retention test failed before the repair.
This establishes capture-boundary risks. It does not prove the cause of each reported missing word.

## Changes

- Request 20 ms capture periods, within the device's reported supported range.
- Confirm that audio has arrived before reporting capture readiness.
- Bound stream creation and the initial sample wait where the backend supports those deadlines.
- On normal stop, collect 200 ms of additional audio before closing the stream.
- Bound the tail wait to one second. A stalled input fails explicitly and still closes the stream.
- Keep cancellation immediate, without a tail.
- Log capture startup and stop durations without logging speech.

The worker keeps draining during the stop tail. It finalizes only after the producer closes.
The microphone remains open during this short tail. Idle mode does not capture audio.
No always-on microphone buffer was added.

## Results

The same isolated source reached readiness in 63 ms after the buffer change.
Stop retained 9600 frames at 48 kHz and completed in 209 ms.
The stalled-input test verified failure within the deadline and a successful next start.

All three isolated capture tests passed. The ordinary checks passed 167 Rust tests, three Python tests, formatting, and Clippy.
The capture timings exclude CLI dispatch, input discovery before recorder startup, model work, and desktop delivery.
Physical microphone startup can differ. The new logs measure it during actual dictation.

Audio spoken before capture starts cannot be recovered by decoder padding.
Use the Listening indication as the readiness cue. This repair reduces the delay before that cue.

## Repeat the isolated check

```bash
NVSTT_BOUNDARY_MODULE=$(pactl load-module module-null-sink sink_name=nvstt_boundary_test)
trap 'pactl unload-module "$NVSTT_BOUNDARY_MODULE"' EXIT
NVSTT_TEST_INPUT=pulseaudio:nvstt_boundary_test.monitor \
  cargo test --lib recorder::boundary_tests -- --ignored --test-threads=1 --nocapture
```

Run the Cargo command through Fedora Distrobox if the host lacks build headers.
Use an isolated test source. These tests deliberately start, stop, and pause capture.

For actual dictation timings:

```bash
journalctl --user -u app-nvstt@autostart.service --since today -g 'audio capture'
```
