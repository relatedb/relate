# Functions Reference

## Aggregation functions

Used in `RETURN` and `WITH` clauses to aggregate across multiple rows.

| Function | Description | Example |
|----------|-------------|---------|
| `count(*)` | Count all rows | `RETURN count(*)` |
| `count(expr)` | Count non-null values | `RETURN count(n.age)` |
| `count(DISTINCT expr)` | Count distinct non-null values | `RETURN count(DISTINCT n.city)` |
| `sum(expr)` | Sum of numeric or duration values | `RETURN sum(n.age)` |
| `avg(expr)` | Average of numeric or duration values | `RETURN avg(n.age)` |
| `min(expr)` | Minimum value | `RETURN min(n.age)` |
| `max(expr)` | Maximum value | `RETURN max(n.age)` |
| `collect(expr)` | Collect values into a list | `RETURN collect(n.name)` |
| `stDev(expr)` | Sample standard deviation (divides by N−1) | `RETURN stDev(n.age)` |
| `stDevP(expr)` | Population standard deviation (divides by N) | `RETURN stDevP(n.age)` |
| `percentileCont(expr, pct)` | Continuous percentile (interpolated) | `RETURN percentileCont(n.age, 0.5)` |
| `percentileDisc(expr, pct)` | Discrete percentile (nearest value) | `RETURN percentileDisc(n.age, 0.75)` |

Every aggregation except `count(*)` (a row count) skips null input values
(ADR-0205): `collect` omits them, `min`/`max`/`avg` over only-null rows
return `null`, and `sum` over only-null (or zero) rows returns `0`.
`sum`/`avg` accept durations and return a duration; mixing numbers and
durations in one call is a type error.

```cypher
UNWIND [1, null, 3] AS x RETURN sum(x)      -- 4
UNWIND [null, null] AS x RETURN sum(x)      -- 0
UNWIND [duration('PT1S'), null, duration('PT2S')] AS x RETURN sum(x)  -- PT3S
```

`stDev` and `stDevP` differ only in the divisor: `stDev` is the sample estimate (divides by N−1), `stDevP` the population value (divides by N). Over the same data they return different numbers:

```cypher
UNWIND [2,4,4,4,5,5,7,9] AS x RETURN stDev(x)   -- 2.138089935299395
UNWIND [2,4,4,4,5,5,7,9] AS x RETURN stDevP(x)  -- 2.0
```

Over an empty sample (no non-null numeric value at all, e.g. `UNWIND [] AS x
RETURN stDev(x)`) both return `null`; over a single value they return `0.0`.
Large magnitudes do not overflow the variance: a sample of the two f64 limits
(`1.7976931348623157E308` and its negation) returns `NaN`, not a finite number
or infinity.

## Scalar functions

### Graph functions

| Function | Description | Example |
|----------|-------------|---------|
| `id(node/rel)` | Internal ID | `RETURN id(n)` |
| `labels(node)` | List of labels | `RETURN labels(n)` |
| `type(rel)` | Relationship type | `RETURN type(r)` |
| `properties(node/rel)` | Map of properties | `RETURN properties(n)` |
| `keys(node/rel/map)` | List of property keys | `RETURN keys(n)` |
| `nodes(path)` | List of nodes in path | `RETURN nodes(p)` |
| `relationships(path)` | List of relationships in path | `RETURN relationships(p)` |
| `length(path)` | Number of relationships in path | `RETURN length(p)` |
| `property_exists(entity, key)` | True if the property exists (Cypher 25; the removed `exists(n.age)` property form is rejected) | `WHERE property_exists(n, 'age')` |

### String functions

| Function | Description | Example |
|----------|-------------|---------|
| `toUpper(s)` | Uppercase | `RETURN toUpper('hello')` → `'HELLO'` |
| `toLower(s)` | Lowercase | `RETURN toLower('HELLO')` → `'hello'` |
| `trim(s)` | Remove surrounding whitespace | `RETURN trim('  hi  ')` → `'hi'` |
| `ltrim(s)` | Remove leading whitespace | `RETURN ltrim('  hi')` → `'hi'` |
| `rtrim(s)` | Remove trailing whitespace | `RETURN rtrim('hi  ')` → `'hi'` |
| `replace(s, old, new)` | Replace all occurrences | `RETURN replace('hello', 'l', 'L')` → `'heLLo'` |
| `split(s, delim)` | Split into list | `RETURN split('a,b,c', ',')` → `['a','b','c']` |
| `substring(s, start[, len])` | Extract substring | `RETURN substring('hello', 1, 3)` → `'ell'` |
| `left(s, n)` | First n characters | `RETURN left('hello', 3)` → `'hel'` |
| `right(s, n)` | Last n characters | `RETURN right('hello', 3)` → `'llo'` |
| `size(s)` | String length | `RETURN size('hello')` → `5` |
| `reverse(s)` | Reverse a string | `RETURN reverse('hello')` → `'olleh'` |

Float `start`/`length` arguments to `substring()`, `left()` and `right()`
follow Neo4j
([ADR-0175](../adr/0175-list-and-string-form-conversions.md)): they
truncate toward zero, NaN converts to `0`, and ±Infinity and out-of-range
values saturate; the converted value must lie in [0, 2147483647] — beyond
it or below 0 raises `NumberOutOfRange`
(`substring('abcdef', head([1.5, 'x']))` → `'bcdef'`,
`left('abcdef', head([2.9, 'x']))` → `'ab'`,
`right('abcdef', head([2.9, 'x']))` → `'ef'`,
`left('abcdef', head([-1.0, 'x']))` → error
`NumberOutOfRange: left() length`). A float *literal* is rejected before
execution (`substring('abcdef', 1.5)` → error
`InvalidArgumentType: substring() start must be an integer`). Any other
non-numeric `start`/`length` — a string, boolean, list or map, whether a
literal or a runtime value — is the same type rejection rather than the
absent-argument default (`substring('abcdef', 'x')` and
`left('abcdef', head(['x', '1']))` → error
`InvalidArgumentType: substring() start must be an integer` /
`left() length must be an integer`); only a genuinely absent argument
keeps the default, and a null one nulls the call.

### Numeric functions

| Function | Description | Example |
|----------|-------------|---------|
| `abs(x)` | Absolute value | `RETURN abs(-5)` → `5` |
| `ceil(x)` | Ceiling | `RETURN ceil(4.2)` → `5` |
| `floor(x)` | Floor | `RETURN floor(4.8)` → `4` |
| `round(x)` | Round to nearest integer | `RETURN round(4.5)` → `5` |
| `sqrt(x)` | Square root | `RETURN sqrt(16)` → `4` |
| `sign(x)` | Sign (-1, 0, or 1) | `RETURN sign(-5)` → `-1` |
| `rand()` | Random float in [0, 1) | `RETURN rand()` |
| `log(x)` | Natural logarithm | `RETURN log(e())` → `1` |
| `log10(x)` | Base-10 logarithm | `RETURN log10(100)` → `2` |
| `exp(x)` | e^x | `RETURN exp(1)` → `2.718...` |
| `e()` | Euler's number | `RETURN e()` → `2.718...` |
| `pi()` | Pi | `RETURN pi()` → `3.14159...` |

### Trigonometric functions

| Function | Description |
|----------|-------------|
| `sin(x)` | Sine (radians) |
| `cos(x)` | Cosine (radians) |
| `tan(x)` | Tangent (radians) |
| `asin(x)` | Arcsine |
| `acos(x)` | Arccosine |
| `atan(x)` | Arctangent |
| `atan2(y, x)` | Two-argument arctangent |
| `degrees(radians)` | Convert radians to degrees |
| `radians(degrees)` | Convert degrees to radians |

### Type conversion functions

| Function | Description | Example |
|----------|-------------|---------|
| `toString(x)` | Convert to string | `RETURN toString(42)` → `'42'` |
| `toInteger(x)` | Convert to integer | `RETURN toInteger('42')` → `42` |
| `toFloat(x)` | Convert to float | `RETURN toFloat('3.14')` → `3.14` |
| `toIntegerOrNull(x)` | Convert to integer; `null` instead of an error | `RETURN toIntegerOrNull('x')` → `null` |
| `toFloatOrNull(x)` | Convert to float; `null` instead of an error | `RETURN toFloatOrNull('x')` → `null` |
| `toBooleanOrNull(x)` | Convert to boolean; `null` instead of an error | `RETURN toBooleanOrNull('nope')` → `null` |
| `toStringOrNull(x)` | Convert to string; `null` instead of a runtime error | `CYPHER 5 RETURN toStringOrNull([1, 2])` → `null` |
| `toBoolean(x)` | Convert to boolean | `RETURN toBoolean('true')` → `true` |
| `toBooleanList(x)` | Convert each list element to a boolean; a non-convertible element becomes `null` | `RETURN toBooleanList(['true', 'x'])` → `[true,null]` |

NaN, Infinity and out-of-range values follow Neo4j
([ADR-0168](../adr/0168-non-finite-and-out-of-range-conversions.md)):

- **`toInteger()` / `toIntegerOrNull()` / `toIntegerList()` on a float**
  truncate toward zero and never fail: NaN converts to `0`, and values at
  or beyond the INTEGER limits, ±Infinity included, saturate to
  `9223372036854775807` / `-9223372036854775808`
  (`toInteger(1.0/0.0)` → `9223372036854775807`). `toIntegerList()` keeps
  INTEGER list elements exact.
- **`toInteger()` on a string** trims it and uses its exact decimal value
  (optional sign, digits with an optional fraction, optional exponent),
  truncated toward zero without float rounding
  (`toInteger('9007199254740993.5')` → `9007199254740993`). A value
  outside the INTEGER range raises
  `NumberOutOfRange: toInteger() …` (`toInteger('1e300')`), where
  `toIntegerOrNull()` returns `null`. No NaN or Infinity spelling is a
  number: `toInteger('NaN')` and `toInteger('Infinity')` are `null`.
- **`toFloat()` / `toFloatOrNull()` on a string** accept exactly `NaN`
  and `Infinity`, optionally preceded by one `+` or `-`, as non-finite
  spellings; every other spelling (`'nan'`, `'inf'`, `'Inf'`,
  `'infinity'`, …) is `null`. A numeric string beyond the float range
  converts to ±Infinity (`toFloat('1e400')` → Infinity).

String list elements and Java string forms follow Neo4j
([ADR-0175](../adr/0175-list-and-string-form-conversions.md)):

- **`toIntegerList()` / `toFloatList()` convert each element
  independently** with the scalar `OrNull` rules: string elements parse
  per the string rules here, float elements per ADR-0168 (NaN → `0`,
  saturate, truncate toward zero), boolean elements convert to `1`/`0` in
  `toIntegerList()` and to `null` in `toFloatList()`, and null elements
  stay null. A string element that fails to parse becomes a `null`
  element — the list conversions never raise for an element:
  `toIntegerList(['1', 'NaN'])` → `[1,null]`,
  `toIntegerList(['1e300'])` → `[null]`,
  `toIntegerList([true, false])` → `[1,0]`,
  `toFloatList([true, 'true'])` → `[null,null]`.
- **Whitespace.** `toInteger()` trims characters ≤ U+0020 before its
  decimal parse and `toFloat()` trims like Java's `Double.parseDouble`;
  both then fall back to the Cypher25 number-literal parser, which trims
  Unicode whitespace: `toFloat(' 1.5 ')` → `1.5`,
  `toInteger('\u00A07\u00A0')` → `7`,
  `toFloat('\u00A01.5\u00A0')` → `1.5`.
- **`f`/`F`/`d`/`D` suffixes.** `toFloat()` accepts one trailing
  float/double suffix (`toFloat('1.5f')` → `1.5`, `toFloat('2f')` →
  `2.0`); `toFloat('2L')` → `null`. `toInteger()` accepts a suffixed
  decimal-double token and truncates (`toInteger('1.5f')` → `1`,
  `toInteger('.5f')` → `0`, `toInteger('1.5e2f')` → `150`), but an
  integer token with a trailing letter is `null` (`toInteger('2f')` →
  `null`, `toInteger('2L')` → `null`).
- **Hex floats.** `toFloat()` accepts Java's hex-float forms
  (`toFloat('0x1p3')` → `8.0`, `toFloat('0x1.8p1')` → `3.0`,
  `toFloat('0X1P3')` → `8.0`); `toInteger()` rejects them
  (`toInteger('0x1p3')` → `null`).
- **Hex/octal/underscore integer literals.** Both conversions accept the
  Cypher25 literal forms: `toInteger('0x1F')` → `31`,
  `toFloat('0x1F')` → `31.0`, `toInteger('-0x1F')` → `-31`,
  `toInteger('0o17')` → `15`, `toInteger('1_000')` → `1000`,
  `toFloat('1_000.5')` → `1000.5`. The hex/octal prefix is
  lowercase-only and hex takes `-` but not `+`: `toInteger('0X1F')` →
  `null`, `toInteger('+0x1F')` → `null`, `toInteger('0x')` → `null`.
- **Exponent overflow.** A numeric string whose decimal exponent exceeds
  the 32-bit range parses as a float literal instead of an exact decimal:
  `toInteger('1e3000000000')` → `9223372036854775807`,
  `toInteger('-1e3000000000')` → `-9223372036854775808`,
  `toInteger('1e-3000000000')` → `0`, `toFloat('1e3000000000')` →
  Infinity. This supersedes ADR-0168's deliberate `'1e3000000000'`
  deviation; every other out-of-range exact decimal keeps ADR-0168's rule
  (`toInteger('1e300')` raises `NumberOutOfRange`,
  `toIntegerOrNull('1e300')` → `null`).

### Type checking functions

| Function | Description | Example |
|----------|-------------|---------|
| `valueType(x)` | Return type name | `RETURN valueType(42)` → `'INTEGER NOT NULL'` |

Returns one of: `INTEGER`, `FLOAT`, `STRING`, `BOOLEAN`, `NULL`, `LIST`, `MAP`, `NODE`, `RELATIONSHIP` (plus `VECTOR`, `UUID`, `DATE`, `TIME`, `LOCAL TIME`, `DATETIME`, `LOCAL DATETIME`, `DURATION`, `POINT`, `PATH`), with the value type's nullability suffix — `valueType(42)` is `INTEGER NOT NULL`, `valueType(null)` is `NULL`, and a list reports its element type (`LIST<INTEGER NOT NULL> NOT NULL`).

### Temporal functions

| Function | Description | Example result |
|----------|-------------|----------------|
| `timestamp()` | Milliseconds since the Unix epoch as an INTEGER, one value for the whole statement, equal to `datetime().epochMillis`; takes no arguments | `1709500000000` |
| `date()` | Current date | `'2024-03-03'` |
| `datetime()` | Current datetime (ISO 8601, zoned) | `'2024-03-03T15:30:00Z'` |
| `time()` | Current zoned time | `'15:30:00Z'` |
| `localtime()` | Current local time | `'15:30:00'` |
| `localdatetime()` | Current local datetime | `'2024-03-03T15:30:00'` |
| `duration(map)` | Construct a duration from a component map | `RETURN duration({days: 1, hours: 2})` → `'P1DT2H'` |

`duration()` accepts any of the `years`/`months`/`weeks`/`days`/`hours`/`minutes`/`seconds` components and normalizes them to an ISO-8601 duration string. For example, `RETURN duration({minutes: 90})` → `'PT1H30M'` and `RETURN duration({months: 14, days: 3, seconds: 30})` → `'P1Y2M3DT30S'`.

### List functions

| Function | Description | Example |
|----------|-------------|---------|
| `size(list)` | List length | `RETURN size([1,2,3])` → `3` |
| `head(list)` | First element | `RETURN head([1,2,3])` → `1` |
| `tail(list)` | All except first | `RETURN tail([1,2,3])` → `[2,3]` |
| `last(list)` | Last element | `RETURN last([1,2,3])` → `3` |
| `reverse(list)` | Reverse order | `RETURN reverse([1,2,3])` → `[3,2,1]` |
| `range(start, end[, step])` | Generate a range | `RETURN range(1, 5)` → `[1,2,3,4,5]` |
| `coalesce(a, b, ...)` | First non-null value | `RETURN coalesce(null, 'hi')` → `'hi'` |
| `reduce(acc = init, x IN list \| expr)` | Reduce a list to a single value | `RETURN reduce(s = 0, x IN [1,2,3] \| s + x)` → `6` |
| `allReduce(acc = init, x IN list \| expr, predicate)` | Reduce with an early-exit predicate; returns whether the predicate held for every iteration | `RETURN allReduce(s = 0, x IN [1,2,3] \| s + x, s < 100)` → `true` |

Float arguments to `range()` follow Neo4j
([ADR-0175](../adr/0175-list-and-string-form-conversions.md)): they
truncate toward zero, NaN converts to `0`, and ±Infinity and out-of-range
values saturate (`range(head([1.5, 'x']), 5)` → `[1,2,3,4,5]`,
`range(head([0.0/0.0, 'x']), 5)` → `[0,1,2,3,4,5]`). A zero step — a NaN
step included — raises `NumberOutOfRange: range() step cannot be 0`
(`range(1, 5, head([0.0/0.0, 'x']))` → error), and a size computation
that overflows the INTEGER range raises `integer out of range`
(`range(1, head([-1.0/0.0, 'x']))` → error, while
`range(5, 1, head([-1.0/0.0, 'x']))` → `[5]`). A float *literal* is
rejected before execution (`range(1.5, 5)` → error
`InvalidArgumentType: range() start`).

List index and slice bounds follow Neo4j
([ADR-0175](../adr/0175-list-and-string-form-conversions.md)): a FLOAT
index — `1.0` and NaN included — raises
`InvalidArgumentType: list index must be an integer`
(`[1,2,3][head([1.0, 'x'])]` → error), while slice bounds truncate toward
zero, convert NaN to `0`, and saturate, and must then fit the 32-bit
range (`[1,2,3][head([1.5, 'x'])..head([2.9, 'x'])]` → `[2]`,
`[1,2,3][head([1.0/0.0, 'x'])..2]` → error
`NumberOutOfRange: slice start`); negative bounds keep the usual
negative-index semantics. A float *literal* in an index or slice bound is
rejected before execution (`[1,2,3][1.0]` → error
`InvalidArgumentType: list index must be an integer`).

### UUID functions

The UUID value type (CIP-246): constructors and 64-bit-half accessors.

| Function | Description | Example |
|----------|-------------|---------|
| `uuid()` | Random UUID (v7-shaped) | `RETURN toString(uuid())` |
| `uuid(s)` | Parse a strict RFC-4122 string | `RETURN uuid('f066ecfc-5151-4121-8a36-b35e00d72918')` |
| `uuid(msb, lsb)` | Build from two 64-bit halves | `RETURN uuid(0, 0)` |
| `uuid.mostSignificantBits(u)` | High 64 bits | `RETURN uuid.mostSignificantBits(uuid('...'))` |
| `uuid.leastSignificantBits(u)` | Low 64 bits | `RETURN uuid.leastSignificantBits(uuid('...'))` |
| `randomUUID()` | Random UUID **as a STRING** | `RETURN valueType(randomUUID())` |

`toString` also renders maps, heterogeneous lists, and graph values in
display form (CIP-271): `toString({a: 1})` → `'{a: 1}'`. The display form is
the cypher-shell one — map keys sorted with a backtick-quoted key outside the
plain identifier shape, node labels sorted and quoted the same way, relationship
types backticked, and temporals in their canonical `HH:MM:SS` form
(`toString(localtime('10:00'))` → `'10:00:00'`).

A list *literal* whose elements all share one statically known type
(`[1, 2]`, `['a', 'b']`, `[1, 2.0]`, `[[1]]`) is a `LIST<T>`, not the
`LIST<ANY>` these functions accept, so `toString()`/`toStringOrNull()`
reject it during semantic analysis (a compile-time `TypeError`). A
dynamically typed list — a property, parameter or variable read — has no
statically known element type and renders normally.

CIP-271 is Cypher-25-only. Under a `CYPHER 5` statement preamble the
pre-CIP-271 contract holds: containers and graph values are a type error for
`toString`, and `toStringOrNull` returns `null` for them.

## Vector functions

The similarity functions below accept a `VECTOR` value (see
[`vector(...)`](#vector-constructor)) **or** a plain list of numbers in each
position; the two arguments must have the same length. `vector_distance`,
`vector_norm` and `vector_dimension_count` take a constructed `VECTOR` only.
Passing `null` for a whole argument returns `null`.

### Similarity functions

These return a **rebounded similarity** in `[0, 1]` where **higher means more
similar** (`1.0` = identical direction/value). They are the functions the vector
index and `db.index.vector.queryNodes` scores are built on — not the raw cosine
of the angle.

| Function | Description | Example |
|----------|-------------|---------|
| `vector.similarity.cosine(a, b)` | Cosine similarity rebounded to `[0, 1]` (`1.0` = same direction, `0.5` = orthogonal, `0.0` = opposite) | `RETURN vector.similarity.cosine([1,0], [0,1])` → `0.5` |
| `vector.similarity.euclidean(a, b)` | `1 / (1 + squaredEuclideanDistance)`, rebounded to `(0, 1]` (`1.0` = identical) | `RETURN vector.similarity.euclidean([0,0], [3,4])` → `0.03846153989434242` |

### Distance functions

These return the **raw metric** value (not rebounded). For every metric,
**lower means closer**: `DOT` returns the negated dot product, so a larger
dot product (more aligned) is a smaller distance.

| Function | Description | Example |
|----------|-------------|---------|
| `vector_distance(v1, v2, metric)` | Vector distance under the `metric` keyword | `RETURN vector_distance(vector([0,0], 2, INT8), vector([3,4], 2, INT8), EUCLIDEAN)` → `5.0` |

`metric` is a bare keyword: `EUCLIDEAN`, `EUCLIDEAN_SQUARED`, `MANHATTAN`,
`COSINE`, `DOT` or `HAMMING`.

```cypher
RETURN vector_distance(vector([0,0], 2, INT8), vector([3,4], 2, INT8), MANHATTAN)
-- 7.0
```

> `vector_distance(..., EUCLIDEAN)` returns the actual distance (`5.0` for
> `[0,0]`→`[3,4]`), whereas `vector.similarity.euclidean` maps that distance
> into a `(0, 1]` similarity (`0.03846…` for the same inputs). Choose the
> distance form for thresholds and the similarity form for ranking.

### Vector utility functions

| Function | Description | Example |
|----------|-------------|---------|
| `vector_dimension_count(v)` | Return vector dimensionality | `RETURN vector_dimension_count(vector([1,2,3], 3, INT8))` → `3` |
| `vector_norm(v, metric)` | Distance from the vector to the origin under `metric` (`EUCLIDEAN` or `MANHATTAN`) | `RETURN vector_norm(vector([3,4], 2, INT8), EUCLIDEAN)` → `5.0` |

### Vector constructor

`vector(list, dimensions, coordinateType)` constructs a first-class
`VECTOR<type>(dim)` value. All **three** arguments are required, and
`coordinateType` is a **bare keyword** (not a string): one of `INT8`/`INTEGER8`,
`INT16`/`INTEGER16`, `INT32`/`INTEGER32`, `FLOAT32`, or the 64-bit
`INT`/`INTEGER`/`FLOAT`.

```cypher
RETURN vector([1, 2, 3], 3, INT8)
-- vector([1, 2, 3], 3, INTEGER8 NOT NULL)

RETURN valueType(vector([1, 2, 3], 3, INT8))
-- "VECTOR<INTEGER8 NOT NULL>(3) NOT NULL"
```

`vector_dimension_count`, `vector_norm` and `vector_distance` take a
constructed `VECTOR`; the similarity helpers also accept a plain list, so a bare
list works wherever a similarity score is wanted.

### Vector search procedure

```cypher
CALL db.index.vector.queryNodes('index_name', k, queryVector)
YIELD node, score
```

Performs top-k nearest neighbor search using a vector index. Returns nodes and their similarity scores, ordered by score descending.

## Geospatial functions

### Point functions

| Function | Description | Example |
|----------|-------------|---------|
| `point(map)` | Create a point | `point({latitude: 40.7, longitude: -74.0})` |
| `point.distance(p1, p2)` | Distance between points | `RETURN point.distance(point({x: 0, y: 0}), point({x: 3, y: 4}))` → `5.0` |
| `point.withinBBox(pt, lowerLeft, upperRight)` | Check if point is within bounding box | `RETURN point.withinBBox(point({x: 1, y: 1}), point({x: 0, y: 0}), point({x: 2, y: 2}))` → `true` |

**Geographic points** use `latitude`/`longitude` keys (and optionally `height` for 3D). Distance is returned in meters using the Haversine formula with an earth radius of 6,378,140 m (matching Neo4j).

**Cartesian points** use `x`/`y` keys (and optionally `z` for 3D). Distance is Euclidean.

Points are the whole geospatial surface: there are no geometry-type
constructors, topological predicates or WKT/GeoJSON conversions in the
Cypher dialect. A radius filter is a distance comparison — `point.distance(p,
center) < radius` — and geometry work belongs in the host language (for
example Shapely from Python), whose values travel in and out as ordinary
properties.

A `POINT INDEX` accelerates both `point.withinBBox(prop, lo, hi)` and
`point.distance(prop, center) < bound` predicates in a `WHERE` clause.

## GQL / Neo4j extensions

Implemented beyond the openCypher function set (Neo4j Cypher 25 / GQL alignment):

### String

- `trim(x)`, `trim(BOTH|LEADING|TRAILING trimChar FROM x)` — GQL trim forms
- `normalize(str)`, `normalize(str, NFC|NFD|NFKC|NFKD)` — Unicode normalization; `str IS [NOT] NORMALIZED [form]` predicate
- `char_length(str)` / `character_length(str)` — GQL aliases for `size()` on strings
- `nullIf(a, b)` — returns null if `a = b`, else `a`
- graduated APOC text functions
- the `||` operator — string and list concatenation

### Numeric

- IEEE-conformant math (`ceil`, `floor`, `round`, `sign` over Infinity/NaN), `isNaN(x)`
- hyperbolic trigonometric functions: `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`
- Infinity/NaN literals

### List

- `coll.zip(list1, list2, ...)` (CIP-274) — zip two or more lists into a list of per-index lists, to the length of the longest input, padding missing positions with `null`; a `null` argument returns `null`, fewer than two arguments or a non-list argument is an error. Gated by `RELATE_PREVIEW=coll-zip` (ADR-0497): without the opt-in the name is the ordinary unknown-function error (42N48) and `SHOW FUNCTIONS` lists no row
- `coll.hasAnyOf(list, values)` / `coll.hasAllOf(list, subset)` / `coll.hasNoneOf(list, values)` (CIP-286) — quantifiers over the `IN` membership predicate: `any(x IN values WHERE x IN list)`, `all(x IN subset WHERE x IN list)` (set logic — duplicates in the subset are not new elements; the direction is asymmetric), and `none(x IN values WHERE x IN list)`. Equality is Cypher's (`1 = 1.0`, structural lists/maps, `NaN <> NaN`); a `null` element is an unknown value, so an undecided outcome is `null`, never a fabricated boolean; a `null` argument returns `null`. All three are gated by `RELATE_PREVIEW=coll-has` (ADR-0497) with the same unknown-function disabled form

### Type

- `valueType(x)` — the Cypher type name of a value (e.g. `LIST<INTEGER NOT NULL> NOT NULL`)
- `x IS :: TYPE` / `x IS NOT :: TYPE` — type-predicate expressions
- `elementId(n)` — stable element id string
- `vector(list, dimensions, coordinateType)` — construct a `VECTOR<type>(dim)` value; `valueType` reports the vector type

### Aggregation

- set-quantifiers on aggregates: `count(DISTINCT x)`, `count(ALL x)`, and the same for `sum`/`avg`/`collect`/`stDev`/`percentileCont`/`percentileDisc`
- GQL aggregate name aliases (e.g. `collect_list`, `percentile_cont`, `stdev_samp`); the `stddev_samp`/`stddev_pop` spellings are also reachable, but only under `RELATE_PREVIEW=gql-stddev-aliases` (ADR-0497) — the default dialect advertises and dispatches the canonical `stdev_samp`/`stdev_pop` rows only

## Unlisted functions

These functions are callable in every build and dialect but are deliberately
absent from `SHOW FUNCTIONS` (ADR-0542): they are engine-only helpers outside
the Cypher-25 function corpus the catalogue mirrors.

### `bar(value, min, max, width)`

Renders `value` as a text bar of block characters. The bar is `width` cells
long when `value >= max` and empty when `value <= min`; in between it grows
in steps of one eighth of a cell (`▏▎▍▌▋▊▉█`, floored). `value`, `min` and
`max` are INTEGER or FLOAT, `width` is an INTEGER from 0 to 1024. Any `null`
argument returns `null`. A non-numeric argument is an `InvalidArgumentType`
error; NaN or infinite numbers and `min >= max` are `InvalidArgumentValue`
errors; a `width` outside 0..1024 is `NumberOutOfRange`.

```cypher
RETURN bar(5, 0, 10, 10) AS half   // '█████'
RETURN bar(1.25, 0, 10, 4) AS part // '▌'
```

See the [Cypher Syntax reference](./cypher-syntax.md#neo4j--gql-extensions) for the full extended surface.
