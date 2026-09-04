# Deliver only finalized transcripts

The daemon uses Parakeet Unified streaming inference while listening, but it
delivers text only after the stop toggle finalizes the recognizer stream. It
does not insert interim text into the focused client. This preserves the
existing toggle contract and avoids editing, rollback, and focus races caused
by changing partial results.

## Consequences

- The recognizer and delivery layers remain separate.
- Streaming improves finalization latency without changing the output contract.
- Interim text may be exposed through diagnostics later, but it is not history
  and is never treated as delivered text.
