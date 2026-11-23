### Notes

- CI: the spec-harness lane is now a required regression gate (ADR-0661) — a pull request fails when a scenario fails that the machine-written baseline in the private harness repository does not already carry as failing. The baseline refreshes on every merge to main by construction (a fix-N-break-N swap is red; the passing number cannot go stale), and each PR run checks out the harness at the revision the baseline records, so upstream corpus churn never reds an engine PR. The traffic-light coverage report is unchanged; scenario ids and corpus paths never render in this repository.
