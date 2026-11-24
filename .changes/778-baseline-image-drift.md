### Notes

- A nightly `baseline-image-drift` cadence compares the pinned baseline
  images with Docker Hub and files, or comments on, one issue per moved
  image — Debian republishes `bookworm-slim` every 1–3 weeks,
  security-driven, so a drift is a re-pin candidate rather than a failure.
  The pin grammar (`repo:tag@sha256:<64 hex>`) gets one home
  (`scripts/baseline-pin.sh`), shared by the drift check, its test and the
  pins guard.
