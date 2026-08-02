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

**Parakeet Unified**:
The English 0.6B Parakeet model that supports both offline and buffered
streaming inference.
_Avoid_: Parakeet v3, streaming Parakeet

**Final delivery**:
The single transcript delivered after a dictation stream is finalized. Interim
recognizer text is never inserted into the focused client.
_Avoid_: Partial typing, live insertion
