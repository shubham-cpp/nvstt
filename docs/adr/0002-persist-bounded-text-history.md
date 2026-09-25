# Persist bounded text history without audio

The daemon persists the last ten successful transcripts and their metadata
across restarts. It does not retain audio by default. This gives the CLI a
useful history while limiting disk use and reducing the privacy impact of
stored voice data.

## Consequences

- The history store belongs under the user's XDG state directory.
- History writes need atomic updates and a hard ten-record limit.
- A future audio-retention feature needs a separate, explicit decision.

## Subsequent decision

ADR 0014 supersedes only the no-default-audio sentence above. Text history
still keeps ten successful transcripts. Audio retention is a separate store.
