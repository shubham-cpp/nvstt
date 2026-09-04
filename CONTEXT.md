# Voice dictation

This context defines the language for the local voice-to-text workflow and its
interaction with the focused client.

## Language

**Dictation**:
A user-controlled capture and transcription session that starts and stops by
toggle commands.
_Avoid_: Recording, voice command

**Transcript**:
The text produced from one completed dictation session.
_Avoid_: Transcription text, result text

**Transcription**:
The act of converting captured speech into a transcript.
_Avoid_: Recognition, delivery

**Finalization latency**:
The time from the stop toggle to the final transcript. It does not include
delivery to the focused client.
_Avoid_: Transcription time, end-to-end delay

**Delivery**:
The attempt to insert a transcript into the client that has keyboard focus.
_Avoid_: Typing, output

**History record**:
A persisted transcript and its session metadata. The system retains the last
ten records. A record exists after transcription succeeds, even when delivery
fails.
_Avoid_: Recording, log entry

**History retention**:
The bounded storage of transcript text and metadata across daemon restarts.
Audio is not retained by default.
_Avoid_: Recording archive, audio history

**Focused client**:
The application surface that owns keyboard focus when delivery occurs.
_Avoid_: Active window, target window

**Delivery fallback**:
A user-visible path that provides the transcript when automatic delivery is
unavailable, such as clipboard copy or an explicit paste action.
_Avoid_: Backup typing, emergency output

**Native-first delivery**:
A delivery policy that tries compositor-authorized input first and uses a
fallback when that path is unavailable.
_Avoid_: Universal typing, forced injection

**Delivery authorization**:
User permission for the daemon to send keyboard input through the desktop
session. The daemon requests it only when automatic delivery is first needed.
_Avoid_: Startup permission, typing privilege

**Voice daemon**:
The long-running local service that owns dictation state, audio capture,
transcription, delivery, history, and notifications.
_Avoid_: Server process, background app

**Streaming recognizer**:
A recognizer that consumes audio chunks during a dictation and produces
intermediate and final decoding state before the user stops listening.
_Avoid_: Live typer, partial delivery

**Native streaming**:
A recognizer path that retains model context while it processes each incoming
stream frame. It does not use independent fixed-duration ASR chunks.
_Avoid_: Five-second chunking, buffered output

**Parakeet Unified**:
The English 0.6B Parakeet model that supports both offline and buffered
streaming inference.
_Avoid_: Parakeet v3, streaming Parakeet

**Nemotron streaming**:
The English 0.6B cache-aware FastConformer-RNNT model used for native
streaming dictation.
_Avoid_: Nemotron batch model, offline Nemotron

**Streaming profile**:
The model-specific streaming latency profile, such as 560 ms. It defines an
official model artifact and can change accuracy and finalization latency.
_Avoid_: Model speed, chunk size

**Speech gate**:
The local Silero VAD stage that forwards detected speech, pre-roll, and a
short bridge to the streaming recognizer.
_Avoid_: Silence auto-finish, transcript filter

**No-speech outcome**:
A successful dictation result when the speech gate finds no speech. It creates
no delivery attempt and no history record.
_Avoid_: Empty transcript failure, silent delivery

**Filled pause**:
A spoken hesitation word such as "uh" or "um". It is not silence. The speech
gate does not remove it.
_Avoid_: Pause, silence, disfluency

**Final delivery**:
The single transcript delivered after a dictation stream is finalized. Interim
recognizer text is never inserted into the focused client.
_Avoid_: Partial typing, live insertion

**Dictation content**:
The transcript after stutter collapse, filled-pause cleanup, replacements, and
optional inverse text normalization. History and final delivery use this text.
A dictation that contains only filled pauses uses the no-speech outcome.
_Avoid_: Raw ASR text, cleaned result

**Stutter collapse**:
Reducing a run of the same 1 or 2 letter token that the recognizer repeated.
It is an ASR artifact, not a filled pause.
_Avoid_: Filler strip, repetition filter

**Replacement**:
A user-defined token substitution applied to dictation content after stutter
collapse and filled-pause cleanup.
_Avoid_: Vocabulary hint, dictionary correction

**Inverse text normalization**:
Conversion of spoken forms such as "twenty one" to written forms such as
"21". It runs after replacements. It does not change a bare "second".
_Avoid_: Punctuation restoration, ASR correction
