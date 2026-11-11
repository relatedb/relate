# 0175. String list elements, Java string forms, and remaining float-to-integer sites follow Neo4j

- **Status**: Accepted
- **Date**: 2026-10-06
- **Deciders**: CypherLite maintainers (Linear LABS-178)

## Context

ADR-0168 settled non-finite and out-of-range numeric conversions for the
scalar `toInteger()` / `toFloat()` paths and deferred three follow-ups
(its Consequences): the string elements of `toIntegerList()` /
`toFloatList()` (its two `in_scope: false` probe rows — the last two
expressions of its probe command — which CypherLite always converts to
null), the Java-parser string forms (whitespace, `f`/`F`/`d`/`D`
suffixes, hex floats, hex/octal/underscore integer literals, and the
exponent-overflow fallback behind its deliberate `'1e3000000000'`
deviation), and an audit of the other float-to-integer sites (`range()`,
`substring()` / `left()` / `right()`, `SKIP` / `LIMIT`, list index and
slice, and arithmetic at the i64 limits). Linear LABS-178 tracks all
three.

The semantics below were verified by direct probe against
`neo4j:2026.09-community` in docker — 213 rows: 30 list-element rows,
106 string-form rows, 77 other-site rows — with this reproducible
command:

```sh
#!/usr/bin/env bash
# LABS-178 probe: reproduces every Neo4j 2026.09 cell of the probe table.
# Needs: docker. Pulls neo4j:2026.09-community if absent, runs a throwaway
# container, probes each expression (as `RETURN <expr> AS v`) and each
# statement (verbatim, marker `STMT`), then removes the container.
# Output: for each row, `### <query>` followed by the result as cypher-shell
# --format plain prints it, or `error <GQLSTATUS> (<legacy code>): <message>`
# with a [compile-time] marker when the FAILURE answers the RUN (no result
# fields were produced).
set -u
C=gbuild-probe-labs178-repro
docker rm -f "$C" >/dev/null 2>&1
docker run -d --name "$C" -e NEO4J_AUTH=neo4j/password123 neo4j:2026.09-community >/dev/null || exit 1
for i in $(seq 1 60); do
  docker exec "$C" cypher-shell -u neo4j -p password123 --format plain 'RETURN 1' </dev/null >/dev/null 2>&1 && break
  sleep 2
done

probe() { # $1 = full query text, $2 = EXPR|STMT
  local q="$1" kind="$2" out ec gql legacy msg first tailpos
  docker exec "$C" rm -f /tmp/cs.log </dev/null
  out=$(docker exec "$C" cypher-shell -u neo4j -p password123 --format plain --log /tmp/cs.log "$q" </dev/null 2>/tmp/probe-stderr.txt)
  ec=$?
  if [ "$ec" -eq 0 ]; then
    if [ "$kind" = "EXPR" ]; then
      printf '%s\n' "$out" | tail -n +2   # drop the header row ("v")
    else
      printf '%s\n' "$out"
    fi
    return
  fi
  gql=$(awk 'NR==1 { if ($0 ~ /^[0-9A-Z][0-9A-Z][0-9A-Z][0-9A-Z][0-9A-Z]: /) print substr($0, 1, 5); exit }' /tmp/probe-stderr.txt)
  # log, scoped to everything after our query's RUN line
  docker exec "$C" cat /tmp/cs.log </dev/null > /tmp/probe-log.txt
  tailpos=$(awk -v q="C: RUN \"$q\"" 'index($0, q) {p=NR} END {print p+0}' /tmp/probe-log.txt)
  legacy=$(awk -v s="$tailpos" 'NR>s && /S: FAILURE /{sub(/.*S: FAILURE /,""); sub(/ .*/,""); print; exit}' /tmp/probe-log.txt)
  msg=$(awk -v s="$tailpos" 'NR>s && /S: FAILURE /{sub(/.*S: FAILURE [^ ]* "/,""); if ($0 ~ /"$/) {sub(/"$/,""); print; exit} print; exit}' /tmp/probe-log.txt)
  if [ -n "$legacy" ]; then
    printf 'error %s (%s): %s' "$gql" "$legacy" "$msg"
    if ! awk -v s="$tailpos" 'NR>s' /tmp/probe-log.txt | grep -q 'S: SUCCESS {fields'; then
      printf ' [compile-time: FAILURE answers RUN, no result fields]'
    fi
    printf '\n'
  else
    first=$(sed -n '1p' /tmp/probe-stderr.txt)
    printf 'error %s [no server FAILURE; client-side]\n' "$first"
  fi
}

while IFS=$(printf '\t') read -r kind text; do
  case "$kind" in
    '#'*) continue ;;
  esac
  if [ "$kind" = "STMT" ]; then
    q="$text"
  else
    q="RETURN $text AS v"
  fi
  echo "### $q"
  probe "$q" "$kind"
done <<'PROBE_ROWS'
#	--- group: list-elements ---
EXPR	toIntegerList(['1', 'NaN'])
EXPR	toFloatList(['1.5', 'NaN'])
EXPR	toIntegerList(['1e300'])
EXPR	toIntegerList(['9223372036854775808', 'x', 1.5])
EXPR	toIntegerList([' 7 ', '2.9', '-0.5'])
EXPR	toFloatList(['Infinity', 'inf', ' 1.5 ', '1e400'])
EXPR	toIntegerList(['0x1F', '1_000', '1.5f'])
EXPR	toFloatList(['0x1p3', '1.5d'])
EXPR	toIntegerList([true, 'true'])
EXPR	toFloatList([true, 'true'])
EXPR	toIntegerList(['1e3000000000'])
EXPR	toIntegerList(['1e400'])
EXPR	toFloatList(['1e3000000000'])
EXPR	toIntegerList(['9223372036854775807.5'])
EXPR	toIntegerList(['9223372036854775807.0'])
EXPR	toFloatList(['0x1F', '1_000', '0o17'])
EXPR	toIntegerList(['nan', 'inf', '-Infinity'])
EXPR	toFloatList(['nan', 'inf', '-Infinity'])
EXPR	toIntegerList([null, '42'])
EXPR	toFloatList([null, '42'])
EXPR	toIntegerList(['+1', '-0', '0001'])
EXPR	toFloatList(['.5', '5.', '+.5', '1e3'])
EXPR	toIntegerList(['.5', '5.', '1e3'])
EXPR	toIntegerList([true, false])
EXPR	toFloatList([true, false])
EXPR	toIntegerList(['\t7\n'])
EXPR	toIntegerList([' 0x1F '])
EXPR	toFloatList([' 0x1p3 '])
EXPR	toIntegerList(['2L', '2f'])
EXPR	toFloatList(['2L', '2f'])
#	--- group: string-forms ---
EXPR	toInteger(' 1.5 ')
EXPR	toFloat(' 1.5 ')
EXPR	toIntegerOrNull(' 1.5 ')
EXPR	toFloatOrNull(' 1.5 ')
EXPR	toInteger('1.5f')
EXPR	toFloat('1.5f')
EXPR	toIntegerOrNull('1.5f')
EXPR	toFloatOrNull('1.5f')
EXPR	toInteger('1.5F')
EXPR	toFloat('1.5F')
EXPR	toIntegerOrNull('1.5F')
EXPR	toFloatOrNull('1.5F')
EXPR	toInteger('1.5d')
EXPR	toFloat('1.5d')
EXPR	toIntegerOrNull('1.5d')
EXPR	toFloatOrNull('1.5d')
EXPR	toInteger('1.5D')
EXPR	toFloat('1.5D')
EXPR	toIntegerOrNull('1.5D')
EXPR	toFloatOrNull('1.5D')
EXPR	toInteger('2f')
EXPR	toFloat('2f')
EXPR	toIntegerOrNull('2f')
EXPR	toFloatOrNull('2f')
EXPR	toInteger('2L')
EXPR	toFloat('2L')
EXPR	toIntegerOrNull('2L')
EXPR	toFloatOrNull('2L')
EXPR	toInteger('0x1F')
EXPR	toFloat('0x1F')
EXPR	toIntegerOrNull('0x1F')
EXPR	toFloatOrNull('0x1F')
EXPR	toInteger('-0x1F')
EXPR	toFloat('-0x1F')
EXPR	toIntegerOrNull('-0x1F')
EXPR	toFloatOrNull('-0x1F')
EXPR	toInteger('0o17')
EXPR	toFloat('0o17')
EXPR	toIntegerOrNull('0o17')
EXPR	toFloatOrNull('0o17')
EXPR	toInteger('017')
EXPR	toFloat('017')
EXPR	toIntegerOrNull('017')
EXPR	toFloatOrNull('017')
EXPR	toInteger('1_000')
EXPR	toFloat('1_000')
EXPR	toIntegerOrNull('1_000')
EXPR	toFloatOrNull('1_000')
EXPR	toInteger('1_000.5')
EXPR	toFloat('1_000.5')
EXPR	toIntegerOrNull('1_000.5')
EXPR	toFloatOrNull('1_000.5')
EXPR	toInteger('0x1p3')
EXPR	toFloat('0x1p3')
EXPR	toIntegerOrNull('0x1p3')
EXPR	toFloatOrNull('0x1p3')
EXPR	toInteger('0x1.8p1')
EXPR	toFloat('0x1.8p1')
EXPR	toIntegerOrNull('0x1.8p1')
EXPR	toFloatOrNull('0x1.8p1')
EXPR	toInteger('1e3000000000')
EXPR	toFloat('1e3000000000')
EXPR	toIntegerOrNull('1e3000000000')
EXPR	toFloatOrNull('1e3000000000')
EXPR	toInteger('-1e3000000000')
EXPR	toFloat('-1e3000000000')
EXPR	toIntegerOrNull('-1e3000000000')
EXPR	toFloatOrNull('-1e3000000000')
EXPR	toInteger('1e-3000000000')
EXPR	toFloat('1e-3000000000')
EXPR	toIntegerOrNull('1e-3000000000')
EXPR	toFloatOrNull('1e-3000000000')
EXPR	toInteger('\t1.5\t')
EXPR	toFloat('\t1.5\t')
EXPR	toIntegerOrNull('\t1.5\t')
EXPR	toFloatOrNull('\t1.5\t')
EXPR	toInteger('\n1.5\n')
EXPR	toFloat('\n1.5\n')
EXPR	toIntegerOrNull('\n1.5\n')
EXPR	toFloatOrNull('\n1.5\n')
EXPR	toInteger('1e-400')
EXPR	toFloat('1e-400')
EXPR	toIntegerOrNull('1e-400')
EXPR	toFloatOrNull('1e-400')
EXPR	toInteger('.5f')
EXPR	toFloat('.5f')
EXPR	toIntegerOrNull('.5f')
EXPR	toFloatOrNull('.5f')
EXPR	toInteger('0x')
EXPR	toFloat('0x')
EXPR	toInteger('0X1F')
EXPR	toFloat('0X1F')
EXPR	toInteger('+0x1F')
EXPR	toFloat('+0x1F')
EXPR	toInteger('0X1P3')
EXPR	toFloat('0X1P3')
EXPR	toInteger('-1.5f')
EXPR	toFloat('-1.5f')
EXPR	toInteger('1.5e2f')
EXPR	toFloat('1.5e2f')
EXPR	toFloat(' Infinity ')
EXPR	toFloat('NaN ')
EXPR	toFloat('\u00A01.5\u00A0')
EXPR	toInteger('\u00A07\u00A0')
EXPR	toInteger('')
EXPR	toFloat('')
#	--- group: other-sites ---
EXPR	range(1.5, 5)
EXPR	range(head([1.5, 'x']), 5)
EXPR	range(head([-2.9, 'x']), 2)
EXPR	range(head([1.0, 'x']), 5)
EXPR	range(1, head([5.9, 'x']))
EXPR	range(1, 10, head([2.5, 'x']))
EXPR	range(1, 5, head([1.0, 'x']))
EXPR	range(head([0.0/0.0, 'x']), 5)
EXPR	range(1, head([0.0/0.0, 'x']))
EXPR	range(1, 5, head([0.0/0.0, 'x']))
EXPR	range(head([1.0/0.0, 'x']), 5)
EXPR	range(1, 5, head([1.0/0.0, 'x']))
EXPR	range(1, head([-1.0/0.0, 'x']))
EXPR	range(5, 1, head([-1.0/0.0, 'x']))
EXPR	range(head([9.3e18, 'x']), head([9.3e18, 'x']))
EXPR	substring('abcdef', 1.5)
EXPR	substring('abcdef', head([1.5, 'x']))
EXPR	substring('abcdef', head([1.0, 'x']))
EXPR	substring('abcdef', head([0.0/0.0, 'x']))
EXPR	substring('abcdef', head([1.0/0.0, 'x']))
EXPR	substring('abcdef', head([-1.0/0.0, 'x']))
EXPR	substring('abcdef', head([-1.5, 'x']))
EXPR	substring('abcdef', head([2147483648.0, 'x']))
EXPR	substring('abcdef', head([9.3e18, 'x']))
EXPR	substring('abcdef', 1, head([2.5, 'x']))
EXPR	substring('abcdef', head([0.0/0.0, 'x']), head([0.0/0.0, 'x']))
EXPR	substring('abcdef', 1, head([1.0/0.0, 'x']))
EXPR	left('abcdef', head([2.9, 'x']))
EXPR	left('abcdef', head([0.0/0.0, 'x']))
EXPR	left('abcdef', head([1.0/0.0, 'x']))
EXPR	left('abcdef', head([-1.0, 'x']))
EXPR	right('abcdef', head([2.9, 'x']))
EXPR	right('abcdef', head([0.0/0.0, 'x']))
EXPR	right('abcdef', head([1.0/0.0, 'x']))
STMT	UNWIND [1,2,3] AS x RETURN x SKIP 1
STMT	UNWIND [1,2,3] AS x RETURN x SKIP 1.0
STMT	UNWIND [1,2,3] AS x RETURN x SKIP 1.5
STMT	UNWIND [1,2,3] AS x RETURN x SKIP 0.0/0.0
STMT	UNWIND [1,2,3] AS x RETURN x SKIP 1.0/0.0
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT 1.0
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT 1.5
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT 0.0/0.0
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT 1.0/0.0
STMT	UNWIND [1,2,3] AS x RETURN x SKIP head([1.5, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x SKIP head([1.0, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x SKIP head([0.0/0.0, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x SKIP head([1.0/0.0, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT head([1.5, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT head([0.0/0.0, 'x'])
STMT	UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0/0.0, 'x'])
EXPR	[1,2,3][1.0]
EXPR	[1,2,3][head([1.0, 'x'])]
EXPR	[1,2,3][head([1.5, 'x'])]
EXPR	[1,2,3][head([0.0/0.0, 'x'])]
EXPR	[1,2,3][1.0..2]
EXPR	[1,2,3][head([1.0, 'x'])..2]
EXPR	[1,2,3][head([1.5, 'x'])..head([2.9, 'x'])]
EXPR	[1,2,3][head([0.0/0.0, 'x'])..2]
EXPR	[1,2,3][1..head([0.0/0.0, 'x'])]
EXPR	[1,2,3][..head([1.5, 'x'])]
EXPR	[1,2,3][head([1.5, 'x'])..]
EXPR	[1,2,3][head([1.0/0.0, 'x'])..2]
EXPR	[1,2,3][head([-1.5, 'x'])..2]
EXPR	[1,2,3][head([2147483648.0, 'x'])..]
EXPR	9223372036854775807 + 1
EXPR	(-9223372036854775807 - 1) - 1
EXPR	9223372036854775807 * 2
EXPR	-(-9223372036854775807 - 1)
EXPR	(-9223372036854775807 - 1) / -1
EXPR	(-9223372036854775807 - 1) % -1
EXPR	1 / 0
EXPR	1 % 0
EXPR	1.0 / 0
EXPR	1.0 / 0.0
EXPR	1.0 % 0.0
EXPR	9223372036854775807 + 0.5
PROBE_ROWS
docker rm -f "$C" >/dev/null
```

The "CypherLite before" column was measured at base commit
`e110fefe171af3c21ee2b9bb2dd7181bd80ea29d` with `cypherlite --mode
jsonl`.

Neo4j's source (branch `2026.09`) explains the probe results:

- `CypherFunctions.toIntegerList` (L3933) and `toFloatList` (L2841) map
  every list element through `toIntegerOrNull` / `toFloatOrNull`, so a
  string element goes through the scalar string parsers and any element
  that fails to parse becomes NULL instead of raising
  ([CypherFunctions.java](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherFunctions.java#L3933)).
- Scalar routing: `toInteger` routes a `TextValue` to the string parser,
  a `NumberValue` to `longValue()` (Java `(long)` cast: NaN → 0,
  ±Infinity and out-of-range saturate) and a `BooleanValue` to 1/0;
  `toFloat` rejects BOOLEAN, so `toFloatOrNull(true)` and
  `toFloatList([true])` are null while `toIntegerList([true])` is `[1]`
  ([CypherFunctions.java
  L2881](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherFunctions.java#L2881)).
- `CypherRuntimeParser.parseAsDoubleOrElseNoValue`
  ([L146](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherRuntimeParser.java#L146))
  tries `java.lang.Double.parseDouble` first (leading/trailing
  whitespace, exact-case `NaN` / `Infinity` with optional sign, hex
  floats like `0x1p3`, `f`/`F`/`d`/`D` suffixes, `.5` / `5.` forms,
  overflow to ±Infinity), then falls back to the Cypher25 number-literal
  parser; anything else is null.
- `CypherRuntimeParser.parseAsLongOrElseNoValue`
  ([L164](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherRuntimeParser.java#L164))
  trims (`String.trim`, chars ≤ U+0020), tries `Long.parseLong`, then a
  `java.math.BigDecimal` with an exact range check against the i64
  limits (out of range throws 22N03, which the `OrNull` paths catch; an
  exponent exceeding the `int` range — `'1e3000000000'` — makes the
  `BigDecimal` constructor itself throw), then falls back to the
  Cypher25 number-literal parser, whose `value().longValue()`
  truncates/saturates.
- The Cypher25 literal tokens behind the fallback
  ([Cypher25Lexer.g4
  L59](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/front-end/parser/v25/parser/src/main/antlr4/org/neo4j/cypher/internal/parser/v25/Cypher25Lexer.g4#L59),
  `LiteralBuilder.exitNumberLiteral`): `DECIMAL_DOUBLE` tolerates a
  trailing identifier, so `'1.5f'` lexes as one double token (hence
  `toInteger('1.5f')` = 1), while an integer token with trailing letters
  (`'2f'`, `'2L'`) throws → null; hex/octal tokens are lowercase `0x` /
  `0o` only (`'0X1F'` → null) and take an optional `-` but not `+`
  (`'+0x1F'` → null); the fallback trims Unicode whitespace, which is
  why `toInteger('\u00A07\u00A0')` = 7 even though `String.trim`
  leaves U+00A0 in place.
- `range()` passes its arguments through `asLong(allowFloats=true)` →
  `NumberValue.longValue()`; step 0 raises 22N03 and a saturated step
  can overflow the size computation (22N28)
  ([CypherFunctions.java
  L529](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherFunctions.java#L529)).
- `substring()` / `left()` / `right()` pass indices through
  `asIntExact(allowFloats=true)`: float → long truncation/saturation,
  then an exact int32 range check — values beyond int32 (including
  ±Infinity) and negatives raise 22N03, NaN → 0
  ([CypherFunctions.java
  L1748](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherFunctions.java#L1748)).
- `SKIP` / `LIMIT` require a static, non-negative INTEGER: float
  literals fail at compile time (42N31), float-typed expressions fail
  the static type check (22NB1), and ANY-typed values reach a runtime
  gate that rejects every float including `1.0` (22N03)
  ([ASTSlicingPhrase.scala
  L92](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/front-end/ast/src/main/scala/org/neo4j/cypher/internal/ast/ASTSlicingPhrase.scala#L92)).
- A list index must be an `IntegralValue` at runtime — any FLOAT index
  including `1.0` and NaN raises 22N01 — while slice bounds use
  `asIntExact` and truncate like `substring()`
  ([CypherFunctions.java
  L3381](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherFunctions.java#L3381)).
- `+`, `-`, `*` and unary minus use `Math.*Exact` → 22N28 on overflow,
  but `Long.MIN_VALUE / -1` uses a plain `/` and silently wraps to
  `Long.MIN_VALUE`, and `Long.MIN_VALUE % -1` = 0; float division/modulo
  follow IEEE (`1.0/0` = Infinity, `1.0%0.0` = NaN)
  ([CypherMath.java
  L63](https://github.com/neo4j/neo4j/blob/2026.09/community/cypher/runtime-util/src/main/java/org/neo4j/cypher/operations/CypherMath.java#L63)).

One structural difference shapes the Decision: Neo4j rejects
statically-FLOAT arguments at the integer-argument sites at *compile
time* (22N27 for `range()` / `substring()` / list index, 22NB1 for
`SKIP` / `LIMIT` expressions and slice bounds, 42N31 for `SKIP` /
`LIMIT` float literals). CypherLite has no static float typing, so that
gate cannot be replicated exactly; the Decision says plainly what
happens instead.

## Decision

Align with Neo4j everywhere the probe settled a value-level behavior;
deviate only on the compile-*phase* of Neo4j's static-FLOAT gate, which
CypherLite cannot express. One rule each:

- **String elements of the list conversions.** `toIntegerList()` and
  `toFloatList()` convert each element independently with the scalar
  `OrNull` rules: string elements parse per the string rules below,
  float elements per ADR-0168 (NaN → 0, saturate, truncate toward zero),
  boolean elements convert to 1/0 in `toIntegerList()` and to null in
  `toFloatList()`, null elements stay null. A string element that fails
  to parse — including an out-of-range numeric string such as `'1e300'`,
  `'9223372036854775808'` or `'9223372036854775807.5'` — becomes a NULL
  element; the list conversions never raise for an element
  (`CypherFunctions.java` L3933/L2841 route every element through the
  `OrNull` paths, which catch the 22N03 the scalar `toInteger()` would
  raise).
- **Whitespace.** `toInteger()` trims characters ≤ U+0020 before its
  decimal parse and `toFloat()` trims per `Double.parseDouble`; both
  then fall back to the Cypher25 number-literal parser, which trims
  Unicode whitespace — so `' 1.5 '`, `'\t1.5\t'` and
  `'\u00A01.5\u00A0'` parse (1 / 1.5) and `toInteger('\u00A07\u00A0')`
  = 7. Non-finite spellings (`NaN`, `Infinity`) are recognized only on
  the ≤ U+0020-trimmed `parseDouble` path; the literal fallback never
  yields a non-finite value.
- **`f`/`F`/`d`/`D` suffixes.** `toFloat()` accepts one trailing
  float/double suffix (`'1.5f'` → 1.5, `'2f'` → 2.0); `'2L'` is null.
  `toInteger()` accepts a suffixed decimal-double token via the literal
  fallback and truncates (`'1.5f'` → 1, `'.5f'` → 0, `'1.5e2f'` → 150),
  but an integer token with a trailing letter is null (`'2f'`, `'2L'`)
  — `DECIMAL_DOUBLE` tolerates the trailing identifier, the integer
  token does not (Cypher25Lexer.g4 L59, `LiteralBuilder`).
- **Hex floats.** `toFloat()` accepts Java's hex-float forms
  (`'0x1p3'` → 8.0, `'0x1.8p1'` → 3.0, `'0X1P3'` → 8.0 — `parseDouble`
  is case-insensitive for `x`/`p` here); `toInteger()` rejects them
  (null), because the fallback's hex-*integer* token is lowercase `0x`
  with hex digits only.
- **Hex/octal/underscore integer literals.** Both conversions accept the
  Cypher25 literal forms via the fallback: `'0x1F'` → 31 / 31.0,
  `'-0x1F'` → -31 / -31.0, `'0o17'` → 15 / 15.0, `'1_000'` → 1000 /
  1000.0, and the underscore decimal `'1_000.5'` → 1000 / 1000.5. The
  hex/octal prefix is lowercase-only and hex takes an optional `-` but
  not `+`: `'0X1F'`, `'+0x1F'` and `'0x'` are null.
- **Exponent overflow.** A numeric string whose decimal exponent exceeds
  the int32 range (`'1e3000000000'`, `'1e-3000000000'`) is parsed as a
  float literal instead of an exact decimal — mirroring the
  `BigDecimal` constructor's `NumberFormatException`, which sends Neo4j
  down the same fallback: `toInteger()` converts the resulting double
  with float semantics (`'1e3000000000'` → 9223372036854775807,
  `'-1e3000000000'` → -9223372036854775808, `'1e-3000000000'` → 0) and
  `toFloat()` yields ±Infinity or 0.0. This **supersedes ADR-0168's
  deliberate `'1e3000000000'` deviation**: that deviation stood because
  the literal fallback was out of scope; with the fallback decided here,
  aligning is strictly simpler and matches the reference. Every other
  out-of-range exact decimal keeps ADR-0168's rule (`'1e300'`, `'1e400'`
  → `NumberOutOfRange` / null).
- **`range()`.** Float arguments convert with Java `(long)`-cast
  semantics — truncate toward zero, NaN → 0, ±Infinity and out-of-range
  saturate (CypherFunctions.java L529/L3485). A zero step, including a
  NaN step, raises `NumberOutOfRange` (`range() step cannot be 0`,
  ADR-0168's 22N03 mapping); a size computation that overflows i64
  raises the arithmetic-overflow error (`integer out of range`,
  22N28-analog) — so `range(1, -Infinity)` errors while
  `range(5, 1, -Infinity)` is `[5]`.
- **`substring()` / `left()` / `right()`.** Float start/length arguments
  truncate toward zero, NaN → 0, and saturate; the result must lie in
  [0, 2147483647] — beyond it (including ±Infinity, `2147483648.0`,
  `9.3e18`) or below 0 raises `NumberOutOfRange`
  (CypherFunctions.java L1748/L3528, `asIntExact`).
- **`SKIP` / `LIMIT`.** Float literals keep their existing parse-time
  error; at runtime ANY float value — including `1.0`, NaN and
  ±Infinity — is rejected with the existing `InvalidArgumentType` error
  (`SKIP/LIMIT must be an integer`); no truncation. This extends the
  current runtime check, which already rejects `1.5` and NaN, to `1.0`
  (ASTSlicingPhrase.scala L92: Neo4j's runtime gate rejects every
  float, 22N03).
- **List index.** Any FLOAT index — including `1.0` and NaN — raises
  `InvalidArgumentType` (`list index must be an integer`); no
  truncation. This extends the current check, which already rejects
  `1.5` and NaN, to `1.0` (CypherFunctions.java L3381: 22N01).
- **List slice.** Bounds truncate toward zero, NaN → 0, and saturate;
  each bound must then fit int32 (else `NumberOutOfRange`), and negative
  bounds keep the existing negative-index semantics
  (CypherFunctions.java L3257–L3296).
- **Arithmetic at the i64 limits.** `+`, `-`, `*` and unary minus keep
  raising `integer out of range` on overflow; integer `/ 0` and `% 0`
  keep their errors; `(-9223372036854775808) / -1` now **wraps** to
  -9223372036854775808 and `(-9223372036854775808) % -1` = 0, following
  Neo4j's plain Java operators, which carry no overflow check on those
  two (CypherMath.java L63, `IntegralValue.dividedBy`); float
  division/modulo keep IEEE semantics.
- **Statically-known floats.** Neo4j's compile-time static-FLOAT gate
  cannot be replicated without a static type system CypherLite does not
  have. CypherLite instead rejects float *literals* at plan time at
  every integer-argument site — `range()`, `substring()` / `left()` /
  `right()`, list index, list-slice bounds — with the same
  `InvalidArgumentType` kind those sites raise at runtime (`SKIP` /
  `LIMIT` float literals already fail at parse time, unchanged), and
  handles every non-literal float at runtime by the rules above. Every
  query that is a float-literal mistake therefore fails before execution
  in both engines; the recorded deviation is that constant float
  *expressions* Neo4j's static gate or constant folder rejects at
  compile time (`SKIP 0.0/0.0`, `SKIP 1.0/0.0`, `LIMIT 0.0/0.0`,
  `LIMIT 1.0/0.0` → 22NB1; `SKIP`/`LIMIT head([1.0/0.0, 'x'])` → 22003,
  the folder refusing an Infinity literal) fail in CypherLite at runtime
  with `InvalidArgumentType` — same outcome (the query errors), different
  phase.

**Error mapping.** No new kinds (ADR-0003 unchanged). Neo4j's
compile-time 22N27 / 22NB1 / 42N31, the runtime 22N01 list-index
TypeError and the 22N03 `SKIP` / `LIMIT` ArgumentError map to
CypherLite's existing `InvalidArgumentType` (a type error, which the
bindings already classify); the 22N03 `substring()` / slice / `range()`
step-zero ArgumentErrors map to `NumberOutOfRange` (ADR-0168's mapping);
22N28 ArithmeticError maps to the existing `integer out of range`
message. The `SKIP` / `LIMIT` float rejection keeps `InvalidArgumentType`
rather than `NumberOutOfRange` because CypherLite already raises it there
for `1.5` and because the condition is a wrong type, not an out-of-range
value.

### Probe results

Where cypher-shell and CypherLite's JSONL render a result differently, it
is the same value: Neo4j `NULL` is `null`; Neo4j `NaN` / `Infinity` /
`-Infinity` are CypherLite's JSON strings `"NaN"` / `"Inf"` / `"-Inf"`
(ADR-0280 — a formatter concern this ADR does not change); Neo4j's
`9.223372036854776E18` is CypherLite's `9.223372036854776e+18`; list
spacing is insignificant. Statement rows (`UNWIND … SKIP/LIMIT`) list
the returned column values for Neo4j (cypher-shell prints a header row
first) and the JSONL lines for CypherLite. CypherLite cells are what
`cypherlite --mode jsonl` prints after `"v":`; an error cell gives the
stderr prefix. **Bold** marks a changed result. `†` marks a cell raised
at plan time (before execution) for a float literal — Neo4j's 22N27 /
22NB1 fire at the same phase. `‡` marks the six compile-vs-runtime
deviation cells named in the Decision. `§` marks the cells that
supersede ADR-0168's deliberate `'1e3000000000'` deviation.

| Expression | Neo4j 2026.09 | CypherLite before | CypherLite after |
| --- | --- | --- | --- |
| `toIntegerList(['1', 'NaN'])` | `[1, NULL]` | `[null,null]` | **`[1,null]`** |
| `toFloatList(['1.5', 'NaN'])` | `[1.5, NaN]` | `[null,null]` | **`[1.5,"NaN"]`** |
| `toIntegerList(['1e300'])` | `[NULL]` | `[null]` | `[null]` |
| `toIntegerList(['9223372036854775808', 'x', 1.5])` | `[NULL, NULL, 1]` | `[null,null,1]` | `[null,null,1]` |
| `toIntegerList([' 7 ', '2.9', '-0.5'])` | `[7, 2, 0]` | `[null,null,null]` | **`[7,2,0]`** |
| `toFloatList(['Infinity', 'inf', ' 1.5 ', '1e400'])` | `[Infinity, NULL, 1.5, Infinity]` | `[null,null,null,null]` | **`["Inf",null,1.5,"Inf"]`** |
| `toIntegerList(['0x1F', '1_000', '1.5f'])` | `[31, 1000, 1]` | `[null,null,null]` | **`[31,1000,1]`** |
| `toFloatList(['0x1p3', '1.5d'])` | `[8.0, 1.5]` | `[null,null]` | **`[8.0,1.5]`** |
| `toIntegerList([true, 'true'])` | `[1, NULL]` | `[null,null]` | **`[1,null]`** |
| `toFloatList([true, 'true'])` | `[NULL, NULL]` | `[null,null]` | `[null,null]` |
| `toIntegerList(['1e3000000000'])` | `[9223372036854775807]` | `[null]` | **`[9223372036854775807]`** |
| `toIntegerList(['1e400'])` | `[NULL]` | `[null]` | `[null]` |
| `toFloatList(['1e3000000000'])` | `[Infinity]` | `[null]` | **`["Inf"]`** |
| `toIntegerList(['9223372036854775807.5'])` | `[NULL]` | `[null]` | `[null]` |
| `toIntegerList(['9223372036854775807.0'])` | `[9223372036854775807]` | `[null]` | **`[9223372036854775807]`** |
| `toFloatList(['0x1F', '1_000', '0o17'])` | `[31.0, 1000.0, 15.0]` | `[null,null,null]` | **`[31.0,1000.0,15.0]`** |
| `toIntegerList(['nan', 'inf', '-Infinity'])` | `[NULL, NULL, NULL]` | `[null,null,null]` | `[null,null,null]` |
| `toFloatList(['nan', 'inf', '-Infinity'])` | `[NULL, NULL, -Infinity]` | `[null,null,null]` | **`[null,null,"-Inf"]`** |
| `toIntegerList([null, '42'])` | `[NULL, 42]` | `[null,null]` | **`[null,42]`** |
| `toFloatList([null, '42'])` | `[NULL, 42.0]` | `[null,null]` | **`[null,42.0]`** |
| `toIntegerList(['+1', '-0', '0001'])` | `[1, 0, 1]` | `[null,null,null]` | **`[1,0,1]`** |
| `toFloatList(['.5', '5.', '+.5', '1e3'])` | `[0.5, 5.0, 0.5, 1000.0]` | `[null,null,null,null]` | **`[0.5,5.0,0.5,1000.0]`** |
| `toIntegerList(['.5', '5.', '1e3'])` | `[0, 5, 1000]` | `[null,null,null]` | **`[0,5,1000]`** |
| `toIntegerList([true, false])` | `[1, 0]` | `[null,null]` | **`[1,0]`** |
| `toFloatList([true, false])` | `[NULL, NULL]` | `[null,null]` | `[null,null]` |
| `toIntegerList(['\t7\n'])` | `[7]` | `[null]` | **`[7]`** |
| `toIntegerList([' 0x1F '])` | `[31]` | `[null]` | **`[31]`** |
| `toFloatList([' 0x1p3 '])` | `[8.0]` | `[null]` | **`[8.0]`** |
| `toIntegerList(['2L', '2f'])` | `[NULL, NULL]` | `[null,null]` | `[null,null]` |
| `toFloatList(['2L', '2f'])` | `[NULL, 2.0]` | `[null,null]` | **`[null,2.0]`** |
| `toInteger(' 1.5 ')` | `1` | `1` | `1` |
| `toFloat(' 1.5 ')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull(' 1.5 ')` | `1` | `1` | `1` |
| `toFloatOrNull(' 1.5 ')` | `1.5` | `null` | **`1.5`** |
| `toInteger('1.5f')` | `1` | `null` | **`1`** |
| `toFloat('1.5f')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('1.5f')` | `1` | `null` | **`1`** |
| `toFloatOrNull('1.5f')` | `1.5` | `null` | **`1.5`** |
| `toInteger('1.5F')` | `1` | `null` | **`1`** |
| `toFloat('1.5F')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('1.5F')` | `1` | `null` | **`1`** |
| `toFloatOrNull('1.5F')` | `1.5` | `null` | **`1.5`** |
| `toInteger('1.5d')` | `1` | `null` | **`1`** |
| `toFloat('1.5d')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('1.5d')` | `1` | `null` | **`1`** |
| `toFloatOrNull('1.5d')` | `1.5` | `null` | **`1.5`** |
| `toInteger('1.5D')` | `1` | `null` | **`1`** |
| `toFloat('1.5D')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('1.5D')` | `1` | `null` | **`1`** |
| `toFloatOrNull('1.5D')` | `1.5` | `null` | **`1.5`** |
| `toInteger('2f')` | `NULL` | `null` | `null` |
| `toFloat('2f')` | `2.0` | `null` | **`2.0`** |
| `toIntegerOrNull('2f')` | `NULL` | `null` | `null` |
| `toFloatOrNull('2f')` | `2.0` | `null` | **`2.0`** |
| `toInteger('2L')` | `NULL` | `null` | `null` |
| `toFloat('2L')` | `NULL` | `null` | `null` |
| `toIntegerOrNull('2L')` | `NULL` | `null` | `null` |
| `toFloatOrNull('2L')` | `NULL` | `null` | `null` |
| `toInteger('0x1F')` | `31` | `null` | **`31`** |
| `toFloat('0x1F')` | `31.0` | `null` | **`31.0`** |
| `toIntegerOrNull('0x1F')` | `31` | `null` | **`31`** |
| `toFloatOrNull('0x1F')` | `31.0` | `null` | **`31.0`** |
| `toInteger('-0x1F')` | `-31` | `null` | **`-31`** |
| `toFloat('-0x1F')` | `-31.0` | `null` | **`-31.0`** |
| `toIntegerOrNull('-0x1F')` | `-31` | `null` | **`-31`** |
| `toFloatOrNull('-0x1F')` | `-31.0` | `null` | **`-31.0`** |
| `toInteger('0o17')` | `15` | `null` | **`15`** |
| `toFloat('0o17')` | `15.0` | `null` | **`15.0`** |
| `toIntegerOrNull('0o17')` | `15` | `null` | **`15`** |
| `toFloatOrNull('0o17')` | `15.0` | `null` | **`15.0`** |
| `toInteger('017')` | `17` | `17` | `17` |
| `toFloat('017')` | `17.0` | `17.0` | `17.0` |
| `toIntegerOrNull('017')` | `17` | `17` | `17` |
| `toFloatOrNull('017')` | `17.0` | `17.0` | `17.0` |
| `toInteger('1_000')` | `1000` | `null` | **`1000`** |
| `toFloat('1_000')` | `1000.0` | `null` | **`1000.0`** |
| `toIntegerOrNull('1_000')` | `1000` | `null` | **`1000`** |
| `toFloatOrNull('1_000')` | `1000.0` | `null` | **`1000.0`** |
| `toInteger('1_000.5')` | `1000` | `null` | **`1000`** |
| `toFloat('1_000.5')` | `1000.5` | `null` | **`1000.5`** |
| `toIntegerOrNull('1_000.5')` | `1000` | `null` | **`1000`** |
| `toFloatOrNull('1_000.5')` | `1000.5` | `null` | **`1000.5`** |
| `toInteger('0x1p3')` | `NULL` | `null` | `null` |
| `toFloat('0x1p3')` | `8.0` | `null` | **`8.0`** |
| `toIntegerOrNull('0x1p3')` | `NULL` | `null` | `null` |
| `toFloatOrNull('0x1p3')` | `8.0` | `null` | **`8.0`** |
| `toInteger('0x1.8p1')` | `NULL` | `null` | `null` |
| `toFloat('0x1.8p1')` | `3.0` | `null` | **`3.0`** |
| `toIntegerOrNull('0x1.8p1')` | `NULL` | `null` | `null` |
| `toFloatOrNull('0x1.8p1')` | `3.0` | `null` | **`3.0`** |
| `toInteger('1e3000000000')` | `9223372036854775807` | error `type error: NumberOutOfRange: toInteger()` … | **`9223372036854775807` §** |
| `toFloat('1e3000000000')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toIntegerOrNull('1e3000000000')` | `9223372036854775807` | `null` | **`9223372036854775807` §** |
| `toFloatOrNull('1e3000000000')` | `Infinity` | `"Inf"` | `"Inf"` |
| `toInteger('-1e3000000000')` | `-9223372036854775808` | error `type error: NumberOutOfRange: toInteger()` … | **`-9223372036854775808` §** |
| `toFloat('-1e3000000000')` | `-Infinity` | `"-Inf"` | `"-Inf"` |
| `toIntegerOrNull('-1e3000000000')` | `-9223372036854775808` | `null` | **`-9223372036854775808` §** |
| `toFloatOrNull('-1e3000000000')` | `-Infinity` | `"-Inf"` | `"-Inf"` |
| `toInteger('1e-3000000000')` | `0` | `0` | `0` |
| `toFloat('1e-3000000000')` | `0.0` | `0.0` | `0.0` |
| `toIntegerOrNull('1e-3000000000')` | `0` | `0` | `0` |
| `toFloatOrNull('1e-3000000000')` | `0.0` | `0.0` | `0.0` |
| `toInteger('\t1.5\t')` | `1` | `1` | `1` |
| `toFloat('\t1.5\t')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('\t1.5\t')` | `1` | `1` | `1` |
| `toFloatOrNull('\t1.5\t')` | `1.5` | `null` | **`1.5`** |
| `toInteger('\n1.5\n')` | `1` | `1` | `1` |
| `toFloat('\n1.5\n')` | `1.5` | `null` | **`1.5`** |
| `toIntegerOrNull('\n1.5\n')` | `1` | `1` | `1` |
| `toFloatOrNull('\n1.5\n')` | `1.5` | `null` | **`1.5`** |
| `toInteger('1e-400')` | `0` | `0` | `0` |
| `toFloat('1e-400')` | `0.0` | `0.0` | `0.0` |
| `toIntegerOrNull('1e-400')` | `0` | `0` | `0` |
| `toFloatOrNull('1e-400')` | `0.0` | `0.0` | `0.0` |
| `toInteger('.5f')` | `0` | `null` | **`0`** |
| `toFloat('.5f')` | `0.5` | `null` | **`0.5`** |
| `toIntegerOrNull('.5f')` | `0` | `null` | **`0`** |
| `toFloatOrNull('.5f')` | `0.5` | `null` | **`0.5`** |
| `toInteger('0x')` | `NULL` | `null` | `null` |
| `toFloat('0x')` | `NULL` | `null` | `null` |
| `toInteger('0X1F')` | `NULL` | `null` | `null` |
| `toFloat('0X1F')` | `NULL` | `null` | `null` |
| `toInteger('+0x1F')` | `NULL` | `null` | `null` |
| `toFloat('+0x1F')` | `NULL` | `null` | `null` |
| `toInteger('0X1P3')` | `NULL` | `null` | `null` |
| `toFloat('0X1P3')` | `8.0` | `null` | **`8.0`** |
| `toInteger('-1.5f')` | `-1` | `null` | **`-1`** |
| `toFloat('-1.5f')` | `-1.5` | `null` | **`-1.5`** |
| `toInteger('1.5e2f')` | `150` | `null` | **`150`** |
| `toFloat('1.5e2f')` | `150.0` | `null` | **`150.0`** |
| `toFloat(' Infinity ')` | `Infinity` | `null` | **`"Inf"`** |
| `toFloat('NaN ')` | `NaN` | `null` | **`"NaN"`** |
| `toFloat('\u00A01.5\u00A0')` | `1.5` | `null` | **`1.5`** |
| `toInteger('\u00A07\u00A0')` | `7` | `7` | `7` |
| `toInteger('')` | `NULL` | `null` | `null` |
| `toFloat('')` | `NULL` | `null` | `null` |
| `range(1.5, 5)` | error 22N27 (compile-time) | error `type error: InvalidArgumentType: range() start` | error `type error: InvalidArgumentType: range() start` † |
| `range(head([1.5, 'x']), 5)` | `[1, 2, 3, 4, 5]` | error `type error: InvalidArgumentType: range() start` | **`[1,2,3,4,5]`** |
| `range(head([-2.9, 'x']), 2)` | `[-2, -1, 0, 1, 2]` | error `type error: InvalidArgumentType: range() start` | **`[-2,-1,0,1,2]`** |
| `range(head([1.0, 'x']), 5)` | `[1, 2, 3, 4, 5]` | error `type error: InvalidArgumentType: range() start` | **`[1,2,3,4,5]`** |
| `range(1, head([5.9, 'x']))` | `[1, 2, 3, 4, 5]` | error `type error: InvalidArgumentType: range() end` | **`[1,2,3,4,5]`** |
| `range(1, 10, head([2.5, 'x']))` | `[1, 3, 5, 7, 9]` | error `type error: InvalidArgumentType: range() step` | **`[1,3,5,7,9]`** |
| `range(1, 5, head([1.0, 'x']))` | `[1, 2, 3, 4, 5]` | error `type error: InvalidArgumentType: range() step` | **`[1,2,3,4,5]`** |
| `range(head([0.0/0.0, 'x']), 5)` | `[0, 1, 2, 3, 4, 5]` | error `type error: InvalidArgumentType: range() start` | **`[0,1,2,3,4,5]`** |
| `range(1, head([0.0/0.0, 'x']))` | `[]` | error `type error: InvalidArgumentType: range() end` | **`[]`** |
| `range(1, 5, head([0.0/0.0, 'x']))` | error 22N03 | error `type error: InvalidArgumentType: range() step` | **error `type error: NumberOutOfRange: range() step cannot be 0`** |
| `range(head([1.0/0.0, 'x']), 5)` | `[]` | error `type error: InvalidArgumentType: range() start` | **`[]`** |
| `range(1, 5, head([1.0/0.0, 'x']))` | `[1]` | error `type error: InvalidArgumentType: range() step` | **`[1]`** |
| `range(1, head([-1.0/0.0, 'x']))` | error 22N28 | error `type error: InvalidArgumentType: range() end` | **error `type error: integer out of range`** |
| `range(5, 1, head([-1.0/0.0, 'x']))` | `[5]` | error `type error: InvalidArgumentType: range() step` | **`[5]`** |
| `range(head([9.3e18, 'x']), head([9.3e18, 'x']))` | `[9223372036854775807]` | error `type error: InvalidArgumentType: range() start` | **`[9223372036854775807]`** |
| `substring('abcdef', 1.5)` | error 22N27 (compile-time) | `"abcdef"` | **error `type error: InvalidArgumentType: substring() start must be an integer` †** |
| `substring('abcdef', head([1.5, 'x']))` | `"bcdef"` | `"abcdef"` | **`"bcdef"`** |
| `substring('abcdef', head([1.0, 'x']))` | `"bcdef"` | `"bcdef"` | `"bcdef"` |
| `substring('abcdef', head([0.0/0.0, 'x']))` | `"abcdef"` | `"abcdef"` | `"abcdef"` |
| `substring('abcdef', head([1.0/0.0, 'x']))` | error 22N03 | `"abcdef"` | **error `type error: NumberOutOfRange: substring() start`** |
| `substring('abcdef', head([-1.0/0.0, 'x']))` | error 22N03 | `"abcdef"` | **error `type error: NumberOutOfRange: substring() start`** |
| `substring('abcdef', head([-1.5, 'x']))` | error 22N03 | `"abcdef"` | **error `type error: NumberOutOfRange: substring() start`** |
| `substring('abcdef', head([2147483648.0, 'x']))` | error 22N03 | `""` | **error `type error: NumberOutOfRange: substring() start`** |
| `substring('abcdef', head([9.3e18, 'x']))` | error 22N03 | `"abcdef"` | **error `type error: NumberOutOfRange: substring() start`** |
| `substring('abcdef', 1, head([2.5, 'x']))` | `"bc"` | `"bcdef"` | **`"bc"`** |
| `substring('abcdef', head([0.0/0.0, 'x']), head([0.0/0.0, 'x']))` | `""` | `"abcdef"` | **`""`** |
| `substring('abcdef', 1, head([1.0/0.0, 'x']))` | error 22N03 | `"bcdef"` | **error `type error: NumberOutOfRange: substring() length`** |
| `left('abcdef', head([2.9, 'x']))` | `"ab"` | `""` | **`"ab"`** |
| `left('abcdef', head([0.0/0.0, 'x']))` | `""` | `""` | `""` |
| `left('abcdef', head([1.0/0.0, 'x']))` | error 22N03 | `""` | **error `type error: NumberOutOfRange: left() length`** |
| `left('abcdef', head([-1.0, 'x']))` | error 22N03 | `""` | **error `type error: NumberOutOfRange: left() length`** |
| `right('abcdef', head([2.9, 'x']))` | `"ef"` | `""` | **`"ef"`** |
| `right('abcdef', head([0.0/0.0, 'x']))` | `""` | `""` | `""` |
| `right('abcdef', head([1.0/0.0, 'x']))` | error 22N03 | `""` | **error `type error: NumberOutOfRange: right() length`** |
| `UNWIND [1,2,3] AS x RETURN x SKIP 1` | `2`, `3` | `{"x":2}`<br>`{"x":3}` | `{"x":2}`<br>`{"x":3}` |
| `UNWIND [1,2,3] AS x RETURN x SKIP 1.0` | error 42N31 (compile-time) | error `syntax error: InvalidArgumentType: SKIP requires an integer value` | error `syntax error: InvalidArgumentType: SKIP requires an integer value` |
| `UNWIND [1,2,3] AS x RETURN x SKIP 1.5` | error 42N31 (compile-time) | error `syntax error: InvalidArgumentType: SKIP requires an integer value` | error `syntax error: InvalidArgumentType: SKIP requires an integer value` |
| `UNWIND [1,2,3] AS x RETURN x SKIP 0.0/0.0` | error 22NB1 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `UNWIND [1,2,3] AS x RETURN x SKIP 1.0/0.0` | error 22NB1 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `UNWIND [1,2,3] AS x RETURN x LIMIT 1.0` | error 42N31 (compile-time) | error `syntax error: InvalidArgumentType: LIMIT requires an integer value` | error `syntax error: InvalidArgumentType: LIMIT requires an integer value` |
| `UNWIND [1,2,3] AS x RETURN x LIMIT 1.5` | error 42N31 (compile-time) | error `syntax error: InvalidArgumentType: LIMIT requires an integer value` | error `syntax error: InvalidArgumentType: LIMIT requires an integer value` |
| `UNWIND [1,2,3] AS x RETURN x LIMIT 0.0/0.0` | error 22NB1 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `UNWIND [1,2,3] AS x RETURN x LIMIT 1.0/0.0` | error 22NB1 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `UNWIND [1,2,3] AS x RETURN x SKIP head([1.5, 'x'])` | error 22N03 | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` |
| `UNWIND [1,2,3] AS x RETURN x SKIP head([1.0, 'x'])` | error 22N03 | `{"x":2}`<br>`{"x":3}` | **error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer`** |
| `UNWIND [1,2,3] AS x RETURN x SKIP head([0.0/0.0, 'x'])` | error 22N03 | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` |
| `UNWIND [1,2,3] AS x RETURN x SKIP head([1.0/0.0, 'x'])` | error 22003 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `UNWIND [1,2,3] AS x RETURN x LIMIT head([1.5, 'x'])` | error 22N03 | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` |
| `UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0, 'x'])` | error 22N03 | `{"x":1}` | **error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer`** |
| `UNWIND [1,2,3] AS x RETURN x LIMIT head([0.0/0.0, 'x'])` | error 22N03 | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` |
| `UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0/0.0, 'x'])` | error 22003 (compile-time) | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` | error `type error: InvalidArgumentType: SKIP/LIMIT must be an integer` ‡ |
| `[1,2,3][1.0]` | error 22N27 (compile-time) | `2` | **error `type error: InvalidArgumentType: list index must be an integer` †** |
| `[1,2,3][head([1.0, 'x'])]` | error 22N01 | `2` | **error `type error: InvalidArgumentType: list index must be an integer`** |
| `[1,2,3][head([1.5, 'x'])]` | error 22N01 | error `type error: InvalidArgumentType: list index must be an integer` | error `type error: InvalidArgumentType: list index must be an integer` |
| `[1,2,3][head([0.0/0.0, 'x'])]` | error 22N01 | error `type error: InvalidArgumentType: list index must be an integer` | error `type error: InvalidArgumentType: list index must be an integer` |
| `[1,2,3][1.0..2]` | error 22NB1 (compile-time) | `[2]` | **error `type error: InvalidArgumentType: slice start must be an integer` †** |
| `[1,2,3][head([1.0, 'x'])..2]` | `[2]` | `[2]` | `[2]` |
| `[1,2,3][head([1.5, 'x'])..head([2.9, 'x'])]` | `[2]` | error `type error: slice start` | **`[2]`** |
| `[1,2,3][head([0.0/0.0, 'x'])..2]` | `[1, 2]` | error `type error: slice start` | **`[1,2]`** |
| `[1,2,3][1..head([0.0/0.0, 'x'])]` | `[]` | error `type error: slice end` | **`[]`** |
| `[1,2,3][..head([1.5, 'x'])]` | `[1]` | error `type error: slice end` | **`[1]`** |
| `[1,2,3][head([1.5, 'x'])..]` | `[2, 3]` | error `type error: slice start` | **`[2,3]`** |
| `[1,2,3][head([1.0/0.0, 'x'])..2]` | error 22N03 | error `type error: slice start` | **error `type error: NumberOutOfRange: slice start`** |
| `[1,2,3][head([-1.5, 'x'])..2]` | `[]` | error `type error: slice start` | **`[]`** |
| `[1,2,3][head([2147483648.0, 'x'])..]` | error 22N03 | `[]` | **error `type error: NumberOutOfRange: slice start`** |
| `9223372036854775807 + 1` | error 22N28 | error `type error: integer out of range` | error `type error: integer out of range` |
| `(-9223372036854775807 - 1) - 1` | error 22N28 | error `type error: integer out of range` | error `type error: integer out of range` |
| `9223372036854775807 * 2` | error 22N28 | error `type error: integer out of range` | error `type error: integer out of range` |
| `-(-9223372036854775807 - 1)` | error 22N28 | error `type error: integer out of range` | error `type error: integer out of range` |
| `(-9223372036854775807 - 1) / -1` | `-9223372036854775808` | error `type error: integer out of range` | **`-9223372036854775808`** |
| `(-9223372036854775807 - 1) % -1` | `0` | error `type error: integer out of range` | **`0`** |
| `1 / 0` | error 22N28 | error `type error: / by zero` | error `type error: / by zero` |
| `1 % 0` | error 22N28 | error `type error: % by zero` | error `type error: % by zero` |
| `1.0 / 0` | `Infinity` | `"Inf"` | `"Inf"` |
| `1.0 / 0.0` | `Infinity` | `"Inf"` | `"Inf"` |
| `1.0 % 0.0` | `NaN` | `"NaN"` | `"NaN"` |
| `9223372036854775807 + 0.5` | `9.223372036854776E18` | `9.223372036854776e+18` | `9.223372036854776e+18` |

## Alternatives considered

- **Propagate the scalar error from list elements** (so
  `toIntegerList(['1e300'])` raises). Rejected: Neo4j routes every
  element through the `OrNull` paths and never raises (CypherFunctions.java
  L3933/L2841) — an erroring list conversion would diverge from the
  reference and destroy data Neo4j keeps (`[1, NULL]`, not an error).
- **Keep ADR-0168's `'1e3000000000'` error.** Rejected: that deviation
  existed only because the literal fallback was out of scope there; with
  the fallback decided for hex/octal/underscore forms, keeping a special
  case for giant exponents is extra code whose sole effect is staying
  divergent.
- **Trim Unicode whitespace once, for both functions.** Rejected: it
  matches every probed row but diverges on the unprobed edges Neo4j's
  two-tier structure pins down — `toFloat()`'s non-finite spellings live
  only on the ≤ U+0020-trimmed `parseDouble` path and the literal
  fallback never yields non-finite values, so
  `toFloat('\u00A0Infinity\u00A0')` is null in Neo4j but would be
  Infinity under a single Unicode trim.
- **Accept uppercase hex integers (`'0X1F'`) or `'+'`-signed hex.**
  Rejected: Neo4j's lexer is lowercase-`0x`-only and its signed-literal
  path takes `-` but not `+`; accepting more turns strings Neo4j rejects
  into numbers and the same import behaves differently in the two
  engines.
- **Runtime-only alignment, no plan-time literal check.** Rejected:
  `range(1.5, 5)` would return `[1,2,3,4,5]` where Neo4j rejects the
  query outright — a float literal at an integer site is almost always a
  bug, CypherLite already rejects `SKIP` / `LIMIT` float literals at
  parse time, and plan-time rejection keeps literal queries failing in
  both engines alike.
- **Replicate Neo4j's static FLOAT type gate in full.** Rejected: it
  needs a static type system CypherLite's planner does not have — an
  ADR-worthy project of its own, disproportionate when the literal check
  plus the runtime rules leave only constant-folded float expressions
  differing, and only in error phase.
- **Keep rejecting every float at runtime at these sites (status
  quo).** Rejected: the probe settled that Neo4j coerces ANY-typed
  floats (truncate toward zero, NaN → 0, saturate) at `range()`,
  `substring()` / `left()` / `right()` and slice bounds; embedded
  callers pass values dynamically, so the runtime path is the common one
  and should match the reference.
- **Truncate `SKIP` / `LIMIT` and list-index floats instead of
  rejecting.** Rejected: Neo4j rejects every float there — including
  `1.0` — with 22N03 / 22N01; truncating would silently accept input the
  reference refuses.
- **Keep erroring on `MIN / -1` and `MIN % -1`.** Rejected: it diverges
  from the reference on a portable expression, the wrap is the natural
  two's-complement result in both the JVM and Rust
  (`wrapping_div` / `wrapping_rem`), and Neo4j parity is the project's
  language floor; the quirk is recorded in Consequences instead.
- **New error kinds for 22N01 / 22N27 / 22NB1.** Rejected: a new kind
  extends the typed error surface ADR-0003 governs for no gain;
  `InvalidArgumentType` already names these sites' type failures and the
  bindings already classify it as a type error.

## Consequences

- `convert.rs` string parsing is restructured to mirror Neo4j's
  pipeline: ≤ U+0020 trim; a `parseLong`-equivalent; the ADR-0168
  exact-decimal parse, now with the int32-exponent gate that diverts
  giant exponents to a float-literal parse; then the shared Cypher25
  literal fallback (lowercase hex/octal, underscores, suffixed decimal
  doubles, Unicode trim). `toFloat()` gains a `parseDouble` emulation
  (hex floats, `f`/`F`/`d`/`D` suffixes, `.5` / `5.` forms, overflow to
  ±Infinity) ahead of the same fallback; ADR-0168's non-finite-spelling
  guard applies to the `parseDouble` path only.
- `toIntegerList()` / `toFloatList()` map elements through the scalar
  `OrNull` paths; boolean elements convert to 1/0 for
  `toIntegerList()` and to null for `toFloatList()`.
- `range()`, `substring()`, `left()`, `right()` and list-slice bounds
  convert float arguments through ADR-0168's shared float→i64 helper
  (truncate toward zero, NaN → 0, saturate); `substring()` /
  `left()` / `right()` add the [0, 2147483647] check and slices the
  int32 check, both raising `NumberOutOfRange`; `range()`'s size
  computation raises `integer out of range` on i64 overflow.
- `SKIP` / `LIMIT` and list index reject every FLOAT value at runtime
  (`InvalidArgumentType`), closing the `1.0` hole; the planner rejects
  float literals at `range()`, `substring()`, `left()`, `right()`, list
  index and slice bounds before execution.
- Integer division and modulo special-case the i64 limits:
  `MIN / -1` wraps to `MIN` and `MIN % -1` = 0. This replicates a Neo4j
  quirk (a silent, mathematically wrong wrap) deliberately, for
  cross-engine portability; `docs/` API text for arithmetic should say
  so when it next touches division.
- Recorded deviation (the `‡` rows): `SKIP 0.0/0.0`, `SKIP 1.0/0.0`,
  `LIMIT 0.0/0.0`, `LIMIT 1.0/0.0` and `SKIP` / `LIMIT
  head([1.0/0.0, 'x'])` fail at compile time in Neo4j (22NB1 / 22003)
  and at runtime in CypherLite (`InvalidArgumentType`). Same outcome,
  different phase; closing it wants a static type system and is not
  planned.
- Unit tests beside `convert.rs` and the affected eval modules pin every
  row of the probe table, with end-to-end coverage through the public
  API; the six `‡` rows assert the runtime error.
- Behavior changes: string elements of the list conversions parse
  instead of yielding null; the Java string forms parse as above;
  `toInteger('1e3000000000')` saturates instead of erroring;
  `range()` / `substring()` / `left()` / `right()` / slices coerce
  floats instead of erroring or mis-truncating; `SKIP` / `LIMIT` and
  list index reject `1.0`; `MIN / -1` wraps. These were wrong answers
  (or, for `MIN / -1`, a deliberate divergence); pre-1.0 this lands as a
  fix, recorded in `CHANGELOG.md` under [Unreleased].
- ADR-0168 is immutable; it gains a **Later references** note pointing
  here, and its `'1e3000000000'` deviation is superseded by this ADR.
- TCK: no pinned scenario covers these conversions; the code change
  verifies the allowlist is unchanged.

## Later references

- ADR-0559 supersedes this ADR's error-mapping note and the probe-table
  cells for the 22N03 rejections discovered while producing rows:
  `range()`'s zero step, the `substring()` / `left()` / `right()` range
  checks and the list-slice bounds now carry a run-time argument variant,
  classified as the `ArgumentError` class under
  `Neo.ClientError.Statement.ArgumentError` at the runtime stage. Their
  rendered text keeps the `NumberOutOfRange` kind token and moves from the
  `type error: ` prefix to `semantic error: `. The `toInteger()` rows above
  move with them for the same reason. The `InvalidArgumentType` rows are
  unchanged.
- ADR-0565 corrects the `toInteger()` part of that note: live Neo4j raises
  its type exception for an out-of-range numeric string (22003, the
  `TypeError` class), so the site keeps the type-class prefix while the
  integer-argument range sites keep the run-time argument class.
- ADR-0569 supersedes the `MIN / -1` half of the consequence above: live
  Neo4j 2026.10.1 raises 22003 for `i64::MIN / -1`, so integer division is
  checked like the other integer arithmetic. `MIN % -1` still answers 0.
