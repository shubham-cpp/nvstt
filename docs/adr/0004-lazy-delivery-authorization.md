# Request delivery authorization lazily

The daemon requests keyboard-only portal authorization on the first automatic
delivery attempt, not during startup. It caches a valid restore token when the
portal supports one and falls back to clipboard delivery when authorization is
denied or unavailable.

## Consequences

- Startup can initialize the model and recorder without a surprising prompt.
- The first delivery has an authorization step and must report its state.
- Restore-token handling and revocation are part of the delivery boundary.
