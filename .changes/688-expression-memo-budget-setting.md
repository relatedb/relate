### Added

- The executor's per-statement memo budget is now an open-time setting:
  `OpenOptions::expression_memo_budget` or the
  `RELATE_EXPRESSION_MEMO_BUDGET` environment variable, in whole MiB
  (default 16, unchanged). `0` disables the budgeted memo tables for a
  no-memo control run; a negative or unparsable value fails the open
  naming the setting. Results never depend on the budget.
