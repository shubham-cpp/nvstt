# Retain recent stopped dictation audio

Save up to seven stopped attempts as private original-rate WAVs with metadata.
Include success, no speech, and failure. Exclude cancel and failed start.
Save before delivery. A storage warning or failure does not change transcription
or delivery.
Keep ten text-history records under [ADR 0002](0002-persist-bounded-text-history.md).
A text record exists after successful transcription, even if delivery fails.
This decision replaces only ADR 0002's no-default-audio rule. Do not change the
default model or speech gate. Do not add power-loss sync to text history.

Design: [Dictation reliability and archive ownership](../superpowers/specs/2026-09-25-dictation-reliability-architecture-design.md).
