# Strip filled pauses from dictation content

Status: accepted

Remove filled pauses such as "uh" and "um" after the recognizer finishes, and
before history and final delivery. Do this in a pure dictation-content module.
Do not put it in the speech gate. Do not wrap the streaming recognizer.

Filled pauses are speech. The speech gate must forward them. They are not
useful dictation text. Evaluation keeps the raw recognizer hypothesis so WER
still measures the model.

A dictation that contains only filled pauses uses the no-speech outcome. It
does not fail transcription. It does not write history. It does not deliver
text.

## Consequences

- The transcript in history and in the focused client has no filled pauses.
- `nvstt model evaluate` still scores raw recognizer text.
- The filler list lives in one module. Callers do not learn it.
- User configuration does not gain a new key in this change.
