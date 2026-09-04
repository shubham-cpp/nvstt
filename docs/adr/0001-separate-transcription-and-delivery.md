# Separate transcription and delivery outcomes

The system records transcription success separately from delivery success. A
transcript enters history when speech-to-text succeeds, even if insertion into
the focused client fails, because Wayland input permissions and compositor
support are independent of recognition. Delivery failures must be reported
clearly and may use a fallback such as clipboard copy.

## Consequences

- History and CLI status need separate transcription and delivery fields.
- Notifications must not claim that text was typed when only transcription
  succeeded.
- The output layer can evolve without changing recognition or history rules.
