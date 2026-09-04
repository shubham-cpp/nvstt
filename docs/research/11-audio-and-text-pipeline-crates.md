# Audio and text pipeline crates

Research date: 2026-09-04.

This note covers three crates for the live dictation path: `rubato` 5.0.0,
`nnnoiseless` 0.5.2, and `text-processing-rs` 0.2.2. It is not a CLI note.
Claims come from crate docs, crate source, and this repository.

## Recommendation

Replace sherpa `LinearResampler` with `rubato` `Fft::<f32>`. Do this first.
Desktop capture is 44.1 kHz or 48 kHz. Linear interpolation aliases energy
above 8 kHz into the speech band. That costs word accuracy on every dictation.

Add `nnnoiseless` as an optional denoise stage in the recognition worker.
Default it off. Score it on noisy clips and on a clean headset before anyone
turns it on. It needs 48 kHz frames and a scale conversion. It must not replace
the Silero speech gate.

Add `text-processing-rs` as an optional inverse-text-normalization (ITN) stage
after stutter collapse, filled-pause cleanup, and `[text.replacements]`.
Default it off until a private corpus shows spoken numbers and dates in real
transcripts. Use `normalize_sentence_with_options` with
`disable_bare_second = true`. Do not use the crate's global `add_rule`.

Do not run resample or denoise in the CPAL callback. `rubato` says that
callback must stay light. The worker already drains PCM. Put the new stages
there.

## Current path

```text
cpal default input (native rate, f32 in [-1, 1], mono downmix)
        |
        v
recognition worker
        |
        +-- speech_gate.rs: sherpa LinearResampler -> 16 kHz -> Silero 512-sample frames
        |
        +-- recognizer.rs:  sherpa LinearResampler -> 16 kHz -> Nemotron / Parakeet
        |
        v
dictation_transcript.rs
  stutter collapse -> filled pauses -> [text.replacements]
        |
        v
history + native-first delivery
```

`nvstt model evaluate` skips the recorder and the dictation-content module. It
feeds WAV samples to the recognizer and scores the raw hypothesis.

Two independent linear resamplers run on the same capture. A single 16 kHz
stage is enough for both the gate and the recognizer.

## rubato

Crate: `rubato` 5.0.0. License: MIT OR Apache-2.0. Owner: Henrik Enquist.
Docs: <https://docs.rs/rubato/5.0.0/rubato/>.

It converts sample rates in chunks. Two real converters exist:

- `Fft`: fixed ratio. FFT, scale the spectrum, inverse FFT, anti-alias window.
  This is the documented default when the two rates do not drift.
- `Async`: variable ratio. Sinc (high quality, high CPU) or polynomial (fast,
  no anti-alias filter). Use this when two device clocks drift.

Capture and the model share one process clock. The ratio is fixed for a
session. Use `Fft`, not `Async`.

`Fft::new(rate_in, rate_out, chunk_size, channels, fixed)` is the constructor.
`fixed` is `FixedSync::Input`, `Output`, or `Both`. For a live drain of unknown
length, `FixedSync::Input` matches "the source gave N frames". Check
`input_frames_next()` and `output_frames_next()` every call. Feed a short last
chunk with `Indexing.partial_len`.

`process_into_buffer` writes into a pre-allocated adapter and does not allocate.
`process` allocates. The worker is not a hardware callback, so either is safe.
Prefer `process_into_buffer` so stop-toggle latency stays predictable.

`output_delay()` reports how many output frames of silence sit at the start.
For a stream, drop that delay once at session start, or accept it as a few
milliseconds of pre-roll. `process_all` trims it for a whole clip. Live
dictation is a stream. Reset with `reset()` on cancel and on a new session.

`f32` implements `Sample`. Stay on `f32` so the rest of the daemon does not
convert to `f64`.

Do not enable the `log` feature. Logging allocates.

### Why this beats LinearResampler

Sherpa's `LinearResampler` interpolates neighbouring samples. Downsampling
48 kHz to 16 kHz without a low-pass filter folds 8–24 kHz energy into 0–8 kHz.
Sibilants and fricatives live in that band. The FFT resampler multiplies an
anti-alias window onto the spectrum before it shortens it.

48 kHz to 16 kHz is an exact 3:1 ratio. GCD is 16000. The minimum FFT block is
tiny (3 input frames). 44.1 kHz to 16 kHz has GCD 100, so the minimum input
block is 441 frames (10 ms). Both are fine for a worker that already sees tens
of milliseconds of PCM per drain.

### Where it goes

Add one resampler owned by the recognition worker, created when the session
sees the capture rate.

1. Drain `AudioSource` at the device rate.
2. Resample to 16 kHz with `Fft::<f32>`.
3. Feed 16 kHz to `SpeechGate` and to `OnlineTransducerRecognizer`.

When the input is already 16 kHz, skip the resampler. The current gate and
recognizer already skip `LinearResampler` in that case.

Do not put `rubato` in `CpalRecorder`. The crate docs use cpal as the example
of "store PCM, resample on another thread".

`evaluation.rs` should use the same 16 kHz conversion for WAV files that are
not 16 kHz. That keeps live dictation and the private corpus on one resampler.

### Suggested type

```rust
Fft::<f32>::new(
    input_hz as usize,
    16_000,
    1024,
    1,
    FixedSync::Input,
)
```

Reuse the instance across chunks. Create a new one only when the capture rate
changes. A rate change during a dictation is already an error.

## nnnoiseless

Crate: `nnnoiseless` 0.5.2. License: BSD-3-Clause. Owner: Joe Neeman.
Repo: <https://github.com/jneem/nnnoiseless>.
Docs: <https://docs.rs/nnnoiseless/0.5.2/nnnoiseless/struct.DenoiseState.html>.

It is a Rust port of Xiph RNNoise. The built-in GRU model ships in the crate.
No extra ONNX file. No network at runtime.

Default Cargo features are `bin` and `dasp`. Those pull clap, hound, and dasp.
For the daemon, disable them:

```toml
nnnoiseless = { version = "0.5.2", default-features = false }
```

That leaves `easyfft` and `once_cell`.

### Frame contract

`FRAME_SIZE` is `120 << 2` = 480 samples. The docs require 48 kHz signed PCM
stored in `f32`, in `[-32768.0, 32767.0]`, not `[-1.0, 1.0]`. One frame is
10 ms. `process_frame` returns a VAD probability as `f32`. The first output
frame has fade-in artifacts. The docs tell you to discard it.

cpal and the rest of nvstt use `f32` in `[-1.0, 1.0]`. Scale by `32767.0`
before `process_frame`. Scale back after. If you skip the scale, the network
sees near-silence and the output is wrong.

`DenoiseState` is large. Keep it in a `Box`. It is `Send` and `Sync`. Create
one per session, or keep one on the worker and reset by replacing it. There is
no `reset()` method. `new()` is the reset.

Carry a remainder buffer. Device callbacks are not 480 samples. On stop, pad
the last partial frame with zeros, process it, then drop the state.

Do not enable the `dasp` `DenoiseSignal` wrapper unless you want that crate's
iterator API. `DenoiseState` is enough.

### What it is good at

RNNoise was trained on stationary noise: fans, HVAC, keyboard rumble, street
hiss. It is cheap on CPU relative to 0.6B ASR. It does not need CUDA.

It can also chew consonants and add musical noise on a clean close-talk
headset. Nemotron already trains on noisy English. Denoise on a quiet mic can
raise WER. That is why the default is off.

Do not use the returned VAD probability as the speech gate. Silero is the
accepted gate. RNNoise VAD is a side product of the denoise net. Mixing the
two policies would make no-speech results harder to explain.

### Where it goes

In the worker, after capture, before Silero:

```text
native-rate mono f32 [-1, 1]
        |
        v
rubato Fft -> 48 kHz          (skip if already 48 kHz)
        |
        v
scale * 32767
nnnoiseless 480-sample frames (discard first 10 ms output)
scale / 32767
        |
        v
rubato Fft -> 16 kHz
        |
        v
SpeechGate + OnlineTransducerRecognizer
```

When denoise is off, one `Fft` from native rate to 16 kHz is enough.

Keep this off the CPAL thread. 480-sample FFT plus GRU is light, but it still
does not belong in the input callback.

BSD-3-Clause needs a copyright notice in distributions. nvstt is MIT. The
licenses combine. Add the RNNoise / nnnoiseless notice next to the binary
license text when denoise ships.

### Config

```toml
[audio]
denoise = false
```

Do not download a model. The weights are in the crate. `nvstt model install`
stays ASR + Silero only.

## text-processing-rs

Crate: `text-processing-rs` 0.2.2. License: Apache-2.0.
Repo: <https://github.com/FluidInference/text-processing-rs>.
Docs: <https://docs.rs/text-processing-rs/0.2.2/text_processing_rs/>.

It is a Rust port of NVIDIA NeMo text processing. Runtime deps are
`lazy_static` only. No FST files to ship. No ONNX. Grammars are compiled in.

ITN maps spoken form to written form:

| Spoken | Written |
| --- | --- |
| two hundred thirty two | 232 |
| five dollars and fifty cents | $5.50 |
| january fifth twenty twenty five | January 5, 2025 |
| quarter past two pm | 02:15 p.m. |
| seventy two degrees fahrenheit | 72 °F |

`normalize("two hundred")` treats the whole string as one expression.
`normalize_sentence("I have twenty one apples")` returns
`"I have 21 apples"`. Dictation is a sentence. Use `normalize_sentence`.

`NormalizeOptions` has three knobs:

- `concat_compound_numbers`: `"seven eighty eight"` -> `"788"`. Default false.
  Leave it false unless aviation-style readback shows up.
- `max_span_tokens`: sliding window, default 16.
- `disable_bare_second`: leave `"second"` as a word in `"give me a second"`.
  Compound ordinals and dates still convert. Set this true for dictation.

The crate reports 98.6% match against NeMo ITN tests (1200/1217). Languages
for sentence mode include `en`, `fr`, `es`, `de`, `zh`, `hi`, `ja`. The ASR
models here are English. Call the English entry point.

### What it does not do

It does not restore punctuation. Nemotron already emits punctuation and
capitalization.

It does not strip `uh` / `um`. That stays in `dictation_transcript.rs`.

It does not apply `[text.replacements]`. `custom_rules::add_rule` is a process
global. Highest priority in sentence mode, shared by every caller. A daemon
with one config file must not use a process-wide rule table. Keep replacements
in `Replacements`.

It does not fix ASR errors. `"to"` vs `"two"` is the recognizer's job.

### False conversions

`"give me a second"` becomes `"give me a 2nd"` unless `disable_bare_second` is
set. That option exists because of issue #22 in the crate.

`"for two hours"` becoming `"for 2 hours"` is usually what a dictation user
wants. Same for money and dates.

ITN after user replacements means a replacement can protect a phrase. A user
who wants the words `"twenty one"` kept can map them to themselves, or we add
a later escape. Do not invent that escape in the first slice.

### Where it goes

`dictation_transcript` already has a fixed order: stutter collapse, filled
pauses, replacements. ADR 0012 froze that order. Insert ITN after
replacements, still inside the same pure function, behind a flag.

```text
raw recognizer text
  -> stutter collapse
  -> drop filled pauses
  -> [text.replacements]
  -> optional normalize_sentence_with_options (disable_bare_second = true)
  -> Ready or NoContent
```

Call site today is `app.rs` after `worker.finish()`. Pass the new flag from
`Config`. Keep the function pure. Do not log the transcript.

`nvstt model evaluate` must not run ITN. ADR 0011 already keeps evaluation on
raw recognizer text so WER measures the model, not cleanup.

Do not call `tn_normalize`. That is written-to-spoken, for TTS.

### Config

```toml
[text]
itn = false

[text.replacements]
"nv stt" = "nvstt"
```

Default false until the private corpus has enough spoken numbers, money, and
dates to see a real gain.

## Combined pipeline

```text
cpal callback: format convert + mono downmix + bounded queue only

worker, per session:
  native PCM
       |
       |  denoise off:  one Fft to 16 kHz
       |  denoise on:   Fft to 48 kHz -> nnnoiseless -> Fft to 16 kHz
       v
  16 kHz mono
       |
       +--> Silero speech gate (unchanged policy)
       +--> Nemotron / Parakeet stream (already 16 kHz, skip LinearResampler)
       v
  final hypothesis
       |
       +--> dictation_transcript (stutter, fillers, replacements, optional ITN)
       v
  history + delivery
```

Cancel still resets gate, recognizer, resampler, and denoise state. No history.
No delivery.

## Evaluation

Add corpus tags for:

- clean close-talk headset
- fan / HVAC
- keyboard
- 44.1 kHz and 48 kHz sources
- spoken numbers, money, dates, `"give me a second"`

Measure four live-path combinations against the same clips:

| denoise | itn | What it tests |
| --- | --- | --- |
| off | off | rubato-only vs today's LinearResampler |
| on | off | noise vs over-suppression |
| off | on | ITN benefit and `"second"` / `"two hours"` |
| on | on | full stack, last |

Do not promote denoise or ITN to default until WER on clean clips does not get
worse, noisy clips get better or stay even, and `"give me a second"` stays a
word. Keep the warmed p95 finalization budget from the Nemotron note: one
second or less after stop.

`rubato` delay is milliseconds. `nnnoiseless` adds 10 ms plus two resample
stages. Neither should break that budget on its own. Measure anyway. The 560 ms
profile was already backlog-bound on this machine.

## Implementation order

1. **rubato.** One 16 kHz converter in the worker. Remove the two
   `LinearResampler` instances once the input to gate and recognizer is 16 kHz.
   Use it in evaluation too. This is the always-on change.
2. **nnnoiseless.** Optional. Config flag. Scale, 480-sample framing, discard
   first frame, pad on stop. Default off.
3. **text-processing-rs.** Optional ITN after replacements.
   `disable_bare_second = true`. Default off. No global `add_rule`.

Do not ship 2 or 3 as default in the same PR as 1. Score each step.

## Out of scope

- Whisper or `transcribe-rs` as a second ASR engine
- RNNoise VAD as a Silero replacement
- sherpa `OnlineSpeechDenoiser` (GTCRN) in this slice. Revisit if RNNoise
  loses on the noisy corpus
- `custom_rules::add_rule` for `[text.replacements]`
- Denoise or ITN inside `nvstt model evaluate` WER, except as an explicit
  extra report column later

## Sources

- [rubato 5.0.0 crate docs](https://docs.rs/rubato/5.0.0/rubato/)
- [rubato `Resampler` trait](https://docs.rs/rubato/5.0.0/rubato/trait.Resampler.html)
- [rubato `Fft`](https://docs.rs/rubato/5.0.0/rubato/struct.Fft.html)
- [nnnoiseless `DenoiseState`](https://docs.rs/nnnoiseless/0.5.2/nnnoiseless/struct.DenoiseState.html)
- [nnnoiseless `denoise.rs` source](https://docs.rs/nnnoiseless/0.5.2/src/nnnoiseless/denoise.rs.html)
- [nnnoiseless `lib.rs` (`FRAME_SIZE`)](https://docs.rs/nnnoiseless/0.5.2/src/nnnoiseless/lib.rs.html)
- [nnnoiseless README](https://github.com/jneem/nnnoiseless)
- [nnnoiseless Cargo features](https://docs.rs/crate/nnnoiseless/latest/features)
- [text-processing-rs crate docs](https://docs.rs/text-processing-rs/0.2.2/text_processing_rs/)
- [text-processing-rs `normalize_sentence`](https://docs.rs/text-processing-rs/0.2.2/text_processing_rs/fn.normalize_sentence.html)
- [text-processing-rs `NormalizeOptions`](https://docs.rs/text-processing-rs/0.2.2/text_processing_rs/options/struct.NormalizeOptions.html)
- [text-processing-rs README](https://github.com/FluidInference/text-processing-rs)
- [NVIDIA Nemotron streaming model card](https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b)
- nvstt `src/recorder.rs`, `src/speech_gate.rs`, `src/recognizer.rs`, `src/dictation_transcript.rs`, `src/app.rs`
- [ADR 0011 filled pauses](../adr/0011-strip-filled-pauses-from-dictation-content.md)
- [ADR 0012 stutter and replacements](../adr/0012-stutter-collapse-and-replacements.md)
- [Nemotron streaming research](./10-nemotron-native-streaming.md)
