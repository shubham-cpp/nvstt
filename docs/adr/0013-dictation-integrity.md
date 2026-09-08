# Preserve dictation integrity

Status: accepted with the integrity repair implementation

This decision supersedes ADR 0011's broad filler-removal guarantee and
ADR 0012's stutter eligibility, token matching, and normalization order.
Other parts of those decisions remain unchanged.

Use one owned consumer and a bounded single-producer, single-consumer queue
for microphone audio. Count known queue loss. Treat reported backend errors
and duration-limit violations as failed capture. Stop the producer before
final integrity checks. Do not store or deliver a partial transcript from
failed capture.

Pre-roll contains only samples not already emitted. Keep detector settings
and one ASR stream per dictation unchanged. Keep the 400 ms pre-roll capacity,
200 ms silence bridge, and 30-second detector setting. The separate capture
session limit remains 30 minutes.

Preserve ambiguous tokens and technical characters during cleanup. Remove
only clear lowercase or title-case `uh` and `um` variants. Limit short-stutter
collapse to runs of at least three copies of the same qualifying token.
Eligible tokens contain one or two ASCII letters. Exclude uppercase
multi-letter acronyms.

Apply replacements after optional inverse text normalization. Patterns see
normalized text when `text.itn = true`. Replacement values receive no later
normalization. Preserve sentence wrappers around nonempty replacement values.
Empty values delete matched tokens and their attached wrappers, not separate
punctuation tokens. Keep original replacement keys when serializing configuration.

Multiword rules cannot consume interior punctuation-only tokens, even when
the configured pattern includes them. Single-token punctuation mappings remain
allowed. For example, `"nv , stt" = "nvstt"` does not match `nv , stt`.
The rule `"," = "comma"` can replace a standalone comma.

## Consequences

- The public CLI, IPC, configuration schemas, and history format stay unchanged.
- The Rust audio source is no longer cloneable; acquisition is single-use.
- Existing replacement files remain readable, but normalized matching can
  require different patterns. Do not rewrite those files automatically.
- `DOT` remains a known ITN limitation. ITN converts it to `.`. This decision
  does not promise general technical-language preservation under ITN.
  Set `text.itn = false` when literal technical wording matters more than
  spoken-number and punctuation conversion. Keep the ITN default unchanged.
- The five-second queue capacity requires paced-input measurement. It is not
  a recording limit or an insertion latency guarantee.
- Unit tests do not prove WER, native-detector behavior, target-window
  correctness, or the stop-to-insertion latency requirement.
