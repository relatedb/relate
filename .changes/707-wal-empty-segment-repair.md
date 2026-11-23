### Fixed

- Opening a store no longer fails with `storage error: truncated WAL
  magic` when the newest WAL segment is a zero-length file: an
  interrupted rotate or store reset that left an empty segment now has
  its header completed on the next open, instead of leaving the store
  unopenable until manually deleted. Sole, closed and
  predecessor-corrupt segments still fail exactly as before, and a
  read-only open or live follower poll now refuses a mismatching short
  newest segment it previously skipped as frameless.
