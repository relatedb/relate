# 0168. Non-finite and out-of-range numeric conversions follow Neo4j

- **Status**: Accepted
- **Date**: 2026-10-06
- **Deciders**: CypherLite maintainers (Linear LABS-177)

## Context

Linear LABS-177 reports a policy divergence inside one function:
`to_integer_value` in `cypherlite-executor/src/eval/function/convert.rs`
saturates floats (`Value::Float(f) => Value::Int(*f as i64)`, so NaN → 0
and ±Infinity → the `i64` limits) while its string path guards
`f.is_finite()` and returns null. The stellar-review of PR #186 raised it
as finding F-1-7/F-2-7 (comparison pass: "policy divergence inside one
function") and deliberately deferred it until a Neo4j reference probe
settled which side is right; the issue's working guess was "non-finite →
null". ADR-0167 is the nearest precedent: `point()` rejects non-finite
coordinates because Neo4j does (22N24) — a probe, not intuition, picked
the rule.

The scope is every float→integer conversion in the conversion builtins —
`toInteger()`, `toIntegerOrNull()`, `toIntegerList()`; NaN, ±Infinity and
finite out-of-range values; float and string arguments — plus the
non-finite string spellings `toFloat()` / `toFloatOrNull()` accept.

The semantics below were verified by direct probe against
`neo4j:2026.09-community` in docker, with this reproducible command:

```sh
C=gbuild-probe-labs177
docker rm -f "$C" >/dev/null 2>&1
docker run -d --name "$C" -e NEO4J_AUTH=neo4j/password123 neo4j:2026.09-community >/dev/null
for i in $(seq 1 45); do docker exec "$C" cypher-shell -u neo4j -p password123 --format plain 'RETURN 1' </dev/null >/dev/null 2>&1 && break; sleep 2; done
while IFS= read -r e; do
  echo "### RETURN $e AS v"
  docker exec "$C" rm -f /tmp/cs.log </dev/null
  docker exec "$C" cypher-shell -u neo4j -p password123 --format plain --log /tmp/cs.log "RETURN $e AS v" </dev/null 2>&1 \
    || docker exec "$C" sh -c "grep -o 'S: FAILURE.*' /tmp/cs.log | tail -n 1" </dev/null
done <<'EOF'
toInteger(0.0/0.0)
toInteger(1.0/0.0)
toInteger(-1.0/0.0)
toInteger(1e300)
toInteger(-1e300)
toInteger(9.3e18)
toInteger(-9.3e18)
toInteger(9.2e18)
toInteger('NaN')
toInteger('nan')
toInteger('Infinity')
toInteger('-Infinity')
toInteger('inf')
toInteger('1e300')
toInteger('9.3e18')
toIntegerOrNull(0.0/0.0)
toIntegerOrNull(1.0/0.0)
toIntegerOrNull(-1.0/0.0)
toIntegerOrNull(1e300)
toIntegerOrNull('NaN')
toIntegerOrNull('1e300')
toIntegerList([0.0/0.0])
toIntegerList([1.0/0.0])
toIntegerList([-1.0/0.0])
toIntegerList([1e300])
toIntegerList([2.5, 9007199254740993])
toFloat('NaN')
toFloat('-NaN')
toFloat('nan')
toFloat('Infinity')
toFloat('-Infinity')
toFloat('+Infinity')
toFloat('infinity')
toFloat('inf')
toFloat('Inf')
toFloat('-inf')
toFloat('1e400')
toFloat(0.0/0.0)
toFloat(1.0/0.0)
toFloatOrNull('NaN')
toFloatOrNull('nan')
toFloatOrNull('Infinity')
toFloatOrNull('inf')
toFloatList([0.0/0.0, 1.0/0.0])
toIntegerList(['1', 'NaN'])
toFloatList(['1.5', 'NaN'])
EOF
docker rm -f "$C" >/dev/null
```

The probe refutes the issue's guess. Neo4j saturates floats exactly like
CypherLite already does — `toInteger()`, `toIntegerOrNull()` and
`toIntegerList()` map NaN to 0 and ±Infinity, ±1e300 and ±9.3e18 to the
`i64` limits — and CIP-52 cip-eval scenarios [11]–[13] pin the same
values (`@neo4j-verified`; the CIP-52 scoreboard in
`baselines/cip-eval.json` is 40/40 and only ratchets up). The real
divergences are elsewhere:

- **Out-of-range numeric strings.** `toInteger('1e300')` and
  `toInteger('9.3e18')` raise GQLSTATUS 22N03 ("specified numeric value out
  of range", cause 22003; legacy `Neo.ClientError.Statement.TypeError`,
  "integer, 1e300, is too large") and `toIntegerOrNull('1e300')` is null;
  CypherLite saturates both.
- **Integer list elements.** `toIntegerList([2.5, 9007199254740993])` is
  `[2, 9007199254740993]` in Neo4j; CypherLite routes every element
  through `f64` and returns `[2, 9007199254740992]`.
- **`toFloat()` spellings.** Neo4j parses with Java's
  `Double.parseDouble`, which knows only `NaN` and `Infinity`, exact case,
  with an optional sign; `'nan'`, `'inf'`, `'Inf'`, `'infinity'` and
  `'-inf'` are null. CypherLite's `str::parse::<f64>` accepts them all,
  case-insensitively.

Neo4j's source explains the string side
(`CypherRuntimeParser.parseAsLongOrElseNoValue`, branch `2026.09`): it
trims, tries `Long.parseLong`, then parses a `java.math.BigDecimal`,
range-checks the *exact* decimal against `Long.MIN_VALUE` ..
`Long.MAX_VALUE` (throwing `CypherTypeException.integerOutOfBounds`, which
`toIntegerOrNull()` catches) and truncates with `longValue()`. Only strings
`BigDecimal` rejects fall back to the Cypher number-literal parser (hex,
octal, underscores) or null; `BigDecimal` has no NaN or Infinity, so no
such spelling is a number for `toInteger()`. Supplementary probes, run the
same way, pin the boundaries this reading predicts (see below).

## Decision

Float→integer conversion keeps Java's `(long) d` / Rust's `as i64`
semantics; string→integer conversion becomes exact-decimal with a range
error; `toIntegerList()` passes integers through; `toFloat()` accepts only
Java's non-finite spellings. One rule each:

- **NaN.** A NaN float converts to `0` — in `toInteger()`,
  `toIntegerOrNull()` and every float element of `toIntegerList()`. No
  string is NaN for integer conversion: every NaN spelling (`'NaN'`,
  `'-NaN'`, `'nan'`, …) is non-numeric and converts to null.
- **±Infinity.** `+Infinity` converts to `9223372036854775807` and
  `-Infinity` to `-9223372036854775808`, scalar and list alike. Every
  Infinity spelling as a string (`'Infinity'`, `'+Infinity'`, `'inf'`, …)
  is non-numeric and converts to null.
- **Finite out-of-range.** A float at or above 2^63 saturates to
  `9223372036854775807`, a float below -2^63 to `-9223372036854775808`; in
  range it truncates toward zero — scalar, `OrNull` and list alike; a float
  never errors. A numeric string (optional sign, digits with an optional
  fraction, optional exponent) is judged by its **exact decimal value**:
  inside [-2^63, 2^63 - 1] it truncates toward zero without `f64` rounding
  (`'9007199254740993.5'` → `9007199254740993`, `'9223372036854775807.0'`
  → `9223372036854775807`); outside it — including strings that overflow
  `f64` such as `'1e400'`, and `'9223372036854775807.5'`, because the check
  precedes truncation — `toInteger()` raises `NumberOutOfRange` and
  `toIntegerOrNull()` returns null. (`toIntegerList()` string elements are
  out of scope; see Consequences.)
- **Integer list elements.** `toIntegerList()` passes the INTEGER elements
  of a list through unchanged; FLOAT elements follow the float rules
  above. VECTOR coordinates keep converting through FLOAT, as Neo4j does
  (`(long) v.doubleValue(i)`), so an INTEGER64 coordinate above 2^53 can
  round, in both engines alike.
- **`toFloat()` spellings.** `toFloat()` / `toFloatOrNull()` accept a
  non-finite spelling only when it is exactly `NaN` or `Infinity`
  (case-sensitive), optionally preceded by one `+` or `-` (`'-NaN'` and
  `'+NaN'` are NaN). Every other spelling Rust's parser accepts — `nan`,
  `NAN`, `inf`, `Inf`, `+inf`, `infinity`, `INFINITY`, … — is null. Numeric
  strings that overflow `f64` still convert to ±Infinity (`'1e400'` →
  Infinity), as `Double.parseDouble` does.

**Error mapping.** Neo4j's 22N03 maps to CypherLite's existing
`NumberOutOfRange` kind, raised through `Error::ty` exactly like
`range()`'s zero step (also 22N03 in Neo4j; supplementary probe). The CLI
prints `type error: NumberOutOfRange: toInteger() expected 'input' to be
of type INTEGER and in the range -9223372036854775808 to
9223372036854775807 but found <input>`, where `<input>` is the trimmed
argument string; tests match the prefix
`type error: NumberOutOfRange: toInteger()`. The bindings classify it as
`TypeError` (`binding_error_metadata` maps every `Error::Type`), matching
Neo4j's legacy `Neo.ClientError.Statement.TypeError`.

### Probe results

Where cypher-shell and CypherLite's JSONL render a result differently, it
is the same value: Neo4j `NULL` is `null`; Neo4j `NaN` / `Infinity` /
`-Infinity` are CypherLite's JSON strings `"NaN"` / `"Inf"` / `"-Inf"`
(ADR-0280 — a formatter concern this ADR does not change); list spacing is
insignificant. Every Neo4j error in this table is 22N03
(`Neo.ClientError.Statement.TypeError`, cause 22003). CypherLite cells are
what `cypherlite --mode jsonl` prints after `"v":`; an error cell gives
the stderr prefix. **Bold** marks a changed result.

| Expression | Neo4j 2026.09 | CypherLite before | CypherLite after |
| --- | --- | --- | --- |
| `toInteger(0.0/0.0)` | `0` | `0` | `0` |
| `toInteger(1.0/0.0)` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toInteger(-1.0/0.0)` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toInteger(1e300)` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toInteger(-1e300)` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toInteger(9.3e18)` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toInteger(-9.3e18)` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toInteger(9.2e18)` | `9200000000000000000` | `9200000000000000000` | `9200000000000000000` |
| `toInteger('NaN')` | `NULL` | `null` | `null` |
| `toInteger('nan')` | `NULL` | `null` | `null` |
| `toInteger('Infinity')` | `NULL` | `null` | `null` |
| `toInteger('-Infinity')` | `NULL` | `null` | `null` |
| `toInteger('inf')` | `NULL` | `null` | `null` |
| `toInteger('1e300')` | error 22N03 | `9223372036854775807` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('9.3e18')` | error 22N03 | `9223372036854775807` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toIntegerOrNull(0.0/0.0)` | `0` | `0` | `0` |
| `toIntegerOrNull(1.0/0.0)` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toIntegerOrNull(-1.0/0.0)` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toIntegerOrNull(1e300)` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toIntegerOrNull('NaN')` | `NULL` | `null` | `null` |
| `toIntegerOrNull('1e300')` | `NULL` | `9223372036854775807` | **`null`** |
| `toIntegerList([0.0/0.0])` | `[0]` | `[0]` | `[0]` |
| `toIntegerList([1.0/0.0])` | `[9223372036854775807]` | `[9223372036854775807]` | `[9223372036854775807]` |
| `toIntegerList([-1.0/0.0])` | `[-9223372036854775808]` | `[-9223372036854775808]` | `[-9223372036854775808]` |
| `toIntegerList([1e300])` | `[9223372036854775807]` | `[9223372036854775807]` | `[9223372036854775807]` |
| `toIntegerList([2.5, 9007199254740993])` | `[2, 9007199254740993]` | `[2,9007199254740992]` | **`[2,9007199254740993]`** |
| `toFloat('NaN')` | `NaN` | `"NaN"` | `"NaN"` |
| `toFloat('-NaN')` | `NaN` | `"NaN"` | `"NaN"` |
| `toFloat('nan')` | `NULL` | `"NaN"` | **`null`** |
| `toFloat('Infinity')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toFloat('-Infinity')` | `-Infinity` | `"-Inf"` | `"-Inf"` |
| `toFloat('+Infinity')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toFloat('infinity')` | `NULL` | `"Inf"` | **`null`** |
| `toFloat('inf')` | `NULL` | `"Inf"` | **`null`** |
| `toFloat('Inf')` | `NULL` | `"Inf"` | **`null`** |
| `toFloat('-inf')` | `NULL` | `"-Inf"` | **`null`** |
| `toFloat('1e400')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toFloat(0.0/0.0)` | `NaN` | `"NaN"` | `"NaN"` |
| `toFloat(1.0/0.0)` | `Infinity` | `"Inf"` | `"Inf"` |
| `toFloatOrNull('NaN')` | `NaN` | `"NaN"` | `"NaN"` |
| `toFloatOrNull('nan')` | `NULL` | `"NaN"` | **`null`** |
| `toFloatOrNull('Infinity')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toFloatOrNull('inf')` | `NULL` | `"Inf"` | **`null`** |
| `toFloatList([0.0/0.0, 1.0/0.0])` | `[NaN, Infinity]` | `["NaN","Inf"]` | `["NaN","Inf"]` |

### Supplementary probes

Run the same way as the probe command (own container, removed afterwards,
same image) to pin the boundaries the rules depend on; they are not part
of the probe table above. The last two rows are error-mapping references
and do not change.

| Expression | Neo4j 2026.09 | CypherLite before | CypherLite after |
| --- | --- | --- | --- |
| `toInteger('-1e300')` | error 22N03 | `-9223372036854775808` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('-9.3e18')` | error 22N03 | `-9223372036854775808` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('1e400')` | error 22N03 | `null` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('-1e400')` | error 22N03 | `null` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger(' 1e300 ')` | error 22N03 | `9223372036854775807` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toIntegerOrNull('1e400')` | `NULL` | `null` | `null` |
| `toIntegerOrNull('9.3e18')` | `NULL` | `9223372036854775807` | **`null`** |
| `toInteger('9223372036854775807')` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toInteger('9223372036854775808')` | error 22N03 | `9223372036854775807` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toIntegerOrNull('9223372036854775808')` | `NULL` | `9223372036854775807` | **`null`** |
| `toInteger('-9223372036854775808')` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toInteger('-9223372036854775809')` | error 22N03 | `-9223372036854775808` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('9223372036854775807.0')` | `9223372036854775807` | `9223372036854775807` | `9223372036854775807` |
| `toInteger('9223372036854775807.5')` | error 22N03 | `9223372036854775807` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('9223372036854775806.5')` | `9223372036854775806` | `9223372036854775807` | **`9223372036854775806`** |
| `toInteger('-9223372036854775808.0')` | `-9223372036854775808` | `-9223372036854775808` | `-9223372036854775808` |
| `toInteger('-9223372036854775808.5')` | error 22N03 | `-9223372036854775808` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('9007199254740993.5')` | `9007199254740993` | `9007199254740994` | **`9007199254740993`** |
| `toInteger('9.2e18')` | `9200000000000000000` | `9200000000000000000` | `9200000000000000000` |
| `toInteger('-0.5')` | `0` | `0` | `0` |
| `toInteger('1e-400')` | `0` | `0` | `0` |
| `toInteger('1e3000000000')` | `9223372036854775807` | `null` | **error `type error: NumberOutOfRange: toInteger()`** |
| `toInteger('+Infinity')` | `NULL` | `null` | `null` |
| `toInteger('-NaN')` | `NULL` | `null` | `null` |
| `toIntegerList([9223372036854775806])` | `[9223372036854775806]` | `[9223372036854775807]` | **`[9223372036854775806]`** |
| `toIntegerList([-9223372036854775807])` | `[-9223372036854775807]` | `[-9223372036854775808]` | **`[-9223372036854775807]`** |
| `toIntegerList([9.3e18, -1e300])` | `[9223372036854775807, -9223372036854775808]` | `[9223372036854775807,-9223372036854775808]` | `[9223372036854775807,-9223372036854775808]` |
| `toIntegerList([9223372036854775807, -9223372036854775808])` | `[9223372036854775807, -9223372036854775808]` | `[9223372036854775807,-9223372036854775808]` | `[9223372036854775807,-9223372036854775808]` |
| `toIntegerList(vector([9007199254740993, 2], 2, INTEGER64))` | `[9007199254740992, 2]` | `[9007199254740992,2]` | `[9007199254740992,2]` |
| `toIntegerList(vector([2.5, 1e300], 2, FLOAT64))` | `[2, 9223372036854775807]` | `[2,9223372036854775807]` | `[2,9223372036854775807]` |
| `toFloat('+NaN')` | `NaN` | `"NaN"` | `"NaN"` |
| `toFloat('NAN')` | `NULL` | `"NaN"` | **`null`** |
| `toFloat('INFINITY')` | `NULL` | `"Inf"` | **`null`** |
| `toFloat('+inf')` | `NULL` | `"Inf"` | **`null`** |
| `toFloat('-1e400')` | `-Infinity` | `"-Inf"` | `"-Inf"` |
| `toFloatOrNull('-Infinity')` | `-Infinity` | `"-Inf"` | `"-Inf"` |
| `toFloatOrNull('Inf')` | `NULL` | `"Inf"` | **`null`** |
| `range(1, 5, 0)` | error 22N03 (ArgumentError) | error `type error: NumberOutOfRange: range() step cannot be 0` | unchanged |
| `9223372036854775807 + 1` | error 22N28 / 22003 (ArithmeticError) | error `type error: integer out of range` | unchanged |

`toInteger('1e3000000000')` is a deliberate deviation: its exponent
overflows `BigDecimal`'s `int` scale, so Neo4j re-parses the string as a
Cypher float literal, which rounds to Infinity and then saturates. That
literal fallback is the path that also serves hex, octal and underscore
forms, which are out of scope; CypherLite applies the exact-value rule and
errors.

## Alternatives considered

- **Non-finite floats → null (the issue's guess).** Rejected: the probe
  refutes it — Neo4j returns `0` and the `i64` limits — and CIP-52
  scenarios [11]–[13] (`@neo4j-verified`, passing today) pin those values;
  null would regress the cip-eval scoreboard and diverge from the
  reference.
- **Keep saturating out-of-range numeric strings.** Rejected: Neo4j raises
  22N03 for `toInteger('1e300')` and returns null from
  `toIntegerOrNull('1e300')`. Saturation silently turns an overflowing
  input into the largest integer — a wrong answer `toIntegerOrNull()`
  cannot even surface. Float saturation stays only because the reference
  saturates floats (Java's `(long)` cast) and CIP-52 pins it.
- **Null instead of an error from `toInteger()` on out-of-range strings.**
  Rejected: null is `toIntegerOrNull()`'s contract; folding the two makes
  the `OrNull` variant pointless for this input, and Neo4j errors. Today's
  null for `'1e400'` is a side effect of the `is_finite()` guard (Rust
  parses it to Infinity), not a policy.
- **Range-check the `f64` parse instead of the exact decimal.** Rejected:
  `f64` cannot represent the boundary — `'9223372036854775807.0'` parses to
  2^63 and would error where Neo4j returns `9223372036854775807`, and
  `'9223372036854775806.5'` would error where Neo4j returns
  `9223372036854775806`; in-range strings round too
  (`'9007199254740993.5'` → `9007199254740994` today). Exact decimal
  parsing is a small digit-string routine with no new dependency.
- **Accept Rust's broader `toFloat()` spellings.** Rejected: `'nan'`,
  `'inf'` and `'infinity'` are null in Neo4j, so accepting them turns data
  Neo4j rejects into values and the same import behaves differently in the
  two engines. Neo4j's spellings are also the ones CypherLite's
  `toString()` emits (`'NaN'`, `'Infinity'`, `'-Infinity'`), so round
  trips keep working.
- **Map 22N03 to `InvalidArgumentValue`** (ADR-0167's kind for non-finite
  coordinates, 22N24). Rejected: `InvalidArgumentValue` is what
  `toInteger({})` raises for a wrong *type*; an out-of-range value is a
  different condition, and CypherLite already names Neo4j's 22N03
  `NumberOutOfRange` (`range()` step zero).
- **Reuse `integer out of range`.** Rejected: that message carries no kind
  prefix, so it classifies as uncategorized, and it is CypherLite's
  arithmetic-overflow error — a different Neo4j error (22N28 / 22003,
  `Neo.ClientError.Statement.ArithmeticError`; supplementary probe).
- **Add a new error kind mirroring Neo4j's `integerOutOfBounds`.**
  Rejected: a new kind extends the typed error surface ADR-0003 governs for
  no gain; `NumberOutOfRange` already names the condition, and its TCK
  class (`ArgumentError`) constrains nothing here because no TCK scenario
  covers this conversion.

## Consequences

- `convert.rs` gains one shared float→`i64` helper — NaN → 0, saturating,
  truncating toward zero (the `as i64` cast, named and documented) — used
  by `toInteger()`, `toIntegerOrNull()` and the `toIntegerList()` element
  projection.
- `to_integer_value`'s string path replaces the `f64` parse with an exact
  decimal parse after the existing trim: in range → the truncated value;
  out of range → `Error::ty("NumberOutOfRange: toInteger() …")`, which
  `toIntegerOrNull()` already maps to null; non-numeric (every NaN and
  Infinity spelling) → null. The TCK TypeConversion2 string rows (`'2.9'`
  → 2, `'1.7'` → 1) are unaffected.
- `convert_list`'s integer projection passes `Value::Int` list elements
  through instead of round-tripping them through `f64`; vector coordinates
  keep the float route.
- `to_float_value` guards its string path so only `NaN` / `Infinity` with
  an optional sign produce non-finite floats; every other spelling is null.
- Unit tests beside `convert.rs` pin the rows of both tables, with
  end-to-end coverage through the public API.
- Behavior change: `toInteger()` on an out-of-range numeric string now
  errors (it saturated, or returned null past `f64` range),
  `toIntegerOrNull()` returns null for it, `toIntegerList()` keeps large
  integers exact, exact in-range decimal strings no longer round, and
  `toFloat('inf')` and the other non-Java spellings return null. These were
  wrong answers; pre-1.0 this lands as a fix, recorded in `CHANGELOG.md`
  under [Unreleased].
- Deferred follow-ups, not decided here:
  - Both `in_scope: false` probe rows differ from Neo4j:
    `toIntegerList(['1', 'NaN'])` is `[1, NULL]` in Neo4j but `[null,null]`
    in CypherLite, and `toFloatList(['1.5', 'NaN'])` is `[1.5, NaN]` in
    Neo4j but `[null,null]`. String elements of the list conversions should
    reuse the string rules decided here.
  - Whitespace, hex/octal/underscore and type-suffix (`'1.5d'`) string
    forms, together with the `'1e3000000000'` deviation above.
  - Other float→integer coercion sites (`range()`, `substring()`,
    `SKIP` / `LIMIT`, arithmetic), `toString()` of NaN and Infinity, and how
    the CLI and formatters print non-finite floats.

## Later references

- ADR-0175 decides the deferred follow-ups above: the string elements of
  `toIntegerList()` / `toFloatList()`, the Java-parser string forms
  (whitespace, `f`/`F`/`d`/`D` suffixes, hex floats, hex/octal/underscore
  integer literals, exponent-overflow fallback) and the other
  float→integer sites (`range()`, `substring()` / `left()` / `right()`,
  `SKIP` / `LIMIT`, list index and slice, arithmetic at the i64 limits).
  This ADR's two `in_scope: false` probe rows are the last two expressions
  of its probe command — `toIntegerList(['1', 'NaN'])` and
  `toFloatList(['1.5', 'NaN'])`, the string list elements — probed but
  absent from its table; ADR-0175's table carries them. ADR-0175 also
  re-evaluates this ADR's deliberate `'1e3000000000'` deviation: with the
  literal fallback in scope, CypherLite aligns with Neo4j and saturates.
- ADR-0559 supersedes this ADR's error mapping for `range()`'s zero step:
  the 22N03 rejections discovered while producing rows now carry a
  run-time argument variant, classified as the `ArgumentError` class under
  `Neo.ClientError.Statement.ArgumentError` at the runtime stage rather
  than the type class this ADR recorded. The migrated rows' rendered text
  keeps the `NumberOutOfRange` kind token; only the variant's class and
  stage move. The `toInteger()` string rows above keep the type class
  this ADR decided.
