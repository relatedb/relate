### Fixed

- `round()`'s precision argument accepts the full int32 range instead of
  erroring above 15: a precision at or above the value's own decimal scale
  returns the value unchanged (live-verified against the reference engine),
  a precision above the int32 range fails naming the bounds, and a negative
  one keeps its ArgumentError with the reference engine's message.
