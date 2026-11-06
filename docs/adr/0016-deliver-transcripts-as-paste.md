# Deliver complete transcripts as paste

Status: accepted

## Problem

Long dictation stalled in Codex CLI 0.160.0 inside Kitty 0.48.2 on KDE Wayland.
Another physical key released another chunk. Transcription and history already contained the complete text.

The old EI backend sent a press and release for every character, plus modifier events.
It reported success when those events reached the EI socket.
That result did not prove the terminal application had consumed them.

An isolated KWin session reproduced the exact symptom without speech recognition.
A 1,395-character fixture left 2,943 bytes unread in the Codex terminal after ten seconds.
Another key released more text. Kitty's raw input receiver consumed the complete stream.
The stall therefore occurred after terminal encoding, in the terminal application's input consumption.

The [upstream Crossterm reader](https://github.com/openai-oss-forks/crossterm/blob/ed1cdab335221515706178d68495bba2aed1924f/src/event/source/unix/mio.rs)
contains a matching readiness hazard. It returns parsed events before fully draining the terminal descriptor.
It can then poll for a new readiness notification while bytes remain unread.
This source inspection supports the observed failure; it does not establish the installed binary's dependency revision.

The burst also produced incorrect capitalization in GTK, Qt, Chromium, and raw Kitty input.
A 1 ms key delay avoided the tested failures. Reading EI replies alone did not fix them.
Neither experiment proves a safe universal typing rate.

## Decision

Deliver the finalized transcript as one paste operation:

1. Set CLIPBOARD and PRIMARY to the same UTF-8 plain text.
2. Wait for both clipboard owners to become ready.
3. Send Shift+Insert through the authorized EI or Wayland keyboard.
4. Release Shift, including after a key-send failure.

Kitty uses PRIMARY for Shift+Insert. Other clients can use CLIPBOARD.
Setting both removes that ambiguity. The terminal supplies bracketed-paste boundaries when its application requests them.
nvstt never types terminal escape sequences into desktop fields.

This replaces character synthesis for all transcript lengths. No transcript chunking, wake-up keys, or timing threshold is used.
The clipboard processes retain the text after the shortcut returns.
The complete transcript remains available for manual paste and in history.

## Failure behavior and limits

- Clipboard preparation must succeed before sending any shortcut.
- A failed native send can be ambiguous. Do not repeat it through another automatic backend.
- Retain the clipboard fallback and the original native error.
- A target must support the standard Shift+Insert paste shortcut.
- Delivery replaces both clipboard selections. It does not restore older clipboard content before an asynchronous reader finishes.
- Protocol success still does not acknowledge text inside an arbitrary application.
- This change avoids the failing terminal input pattern. It does not patch or replace the installed Codex binary.

## Verification

`scripts/check_delivery.py` creates a private D-Bus session, headless KWin, and a private text field or Kitty window.
It uses the production `NativeFirstSink` through `examples/delivery_probe.rs`.
Its `--legacy` option retains the former unpaced sender solely as a regression control.

The Codex check measures unread terminal bytes and requires one complete paste marker before any additional key.
It never submits a prompt. GTK and raw Kitty checks compare exact text, including Unicode and line breaks.
The initial prototype also passed exact comparisons in Qt and Chromium.

The Wayland virtual-keyboard backend uses the same preparation and modifier cleanup logic.
The native end-to-end checks on this host cover the EI backend.
