# Collapse short stutters and apply user replacements

Status: accepted

After filled-pause cleanup, dictation content also:

1. Collapses a run of three or more copies of the same 1 or 2 letter token
   to one copy. This is a Nemotron/Parakeet loop, not a spoken repeat.
   `"no no"` stays. `"hello hello hello"` stays.
2. Applies a user replacement table from `[text.replacements]`. Matching is
   whole-token and case-insensitive. Longer patterns win. The replacement
   text is used as written.

Order is stutter collapse, then filled pauses, then replacements. Evaluation
does not use this path. Optional inverse text normalization can run after this
sequence. Evaluation still scores raw recognizer text.

Do not add `like` or `you know` to the filler list. Those need a later,
optional rewrite stage.

## Consequences

- Users fix names and symbols in config, without an LLM.
- A Parakeet short-token loop does not reach the focused client.
- Empty replacement values delete the matched tokens.
