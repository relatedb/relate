"use strict";

// The typed value codec (ADR-0502): the two directions of the C ABI's typed
// value channel, sharing one vocabulary of value classes.
//
//  - decode: the value walker (relate_value_at + the relate_value_*
//    accessors) turns a result cell into a JS value — one switch over the
//    kind, recursion for the containers, a defined error for an enumerator
//    this client does not know.
//  - encode: the params builder (relate_params_* putters) turns a plain
//    JS params object into a builder tree — the same classes decode produces
//    bind back unchanged.
//
// Pure over the FFI: the module takes the loaded koffi library and returns
// the codec; library loading (path resolution) stays in index.js.

const koffi = require("koffi");
const { decodedEntity } = require("./entity");
const { abiViolation } = require("./errors");

// int64 range and the Number safe-integer window, as BigInts for exact
// comparison.
const INT64_MIN = -9223372036854775808n;
const INT64_MAX = 9223372036854775807n;
const MAX_SAFE = 9007199254740991n;
const MIN_SAFE = -9007199254740991n;

/// The int64 representation policy (LABS-901): a Cypher INTEGER is a JS
/// number when it is exactly representable (within ±2^53-1) and a BigInt
/// otherwise — deterministic by value, never koffi's per-call choice, and
/// never a silently rounded Number. `RETURN 9007199254740993` comes back as
/// exactly `9007199254740993n`.
function jsInt(bi) {
  return bi >= MIN_SAFE && bi <= MAX_SAFE ? Number(bi) : bi;
}

/// Assert a BigInt fits int64 (the putter would refuse it anyway; refusing
/// client-side keeps the message about the JS value).
function asInt64(bi, label) {
  if (bi < INT64_MIN || bi > INT64_MAX) {
    throw new TypeError(`${label} is an integer outside the int64 range`);
  }
  return bi;
}

// --- Typed value classes --------------------------------------------------
//
// The kinds JSON cannot spell (LABS-901's int64 aside): a FLOAT deliberately
// distinct from INTEGER, temporals as lossless components, points, vectors,
// durations as months/days/seconds/nanos. Instances — never plain objects —
// so a user map that happens to carry the same keys is still a map, in both
// directions: decode only ever produces these from a matching walker kind,
// and encode only ever binds them from an instanceof match.

/// A FLOAT-typed parameter value: `float(1)` binds the FLOAT 1.0, not the
/// INTEGER 1. Cypher's `=` compares numerically across the two (1 = 1.0 is
/// true either way), so the type shows up in the rendering and anywhere a
/// FLOAT is required (`toString(float(1))` is `"1.0"`, `toString(1)` is
/// `"1"`).
class CypherFloat {
  constructor(value) {
    this.value = value;
  }
}

/// A DATE as its calendar components.
class CypherDate {
  constructor(year, month, day) {
    this.year = year;
    this.month = month;
    this.day = day;
  }
}

/// A TIME: the local wall clock plus its UTC offset. A TIME keeps no zone id
/// (the engine drops it); binding one is offset-only.
class CypherTime {
  constructor(hour, minute, second, nanosecond, offsetSeconds) {
    this.hour = hour;
    this.minute = minute;
    this.second = second;
    this.nanosecond = nanosecond;
    this.offsetSeconds = offsetSeconds;
  }
}

/// A LOCAL_TIME: the wall clock, no offset.
class CypherLocalTime {
  constructor(hour, minute, second, nanosecond) {
    this.hour = hour;
    this.minute = minute;
    this.second = second;
    this.nanosecond = nanosecond;
  }
}

/// A DATETIME: the local wall clock, its UTC offset, and the IANA zone id
/// when the value keeps one (`null` for an offset-only value).
class CypherDateTime {
  constructor(year, month, day, hour, minute, second, nanosecond, offsetSeconds, zone = null) {
    this.year = year;
    this.month = month;
    this.day = day;
    this.hour = hour;
    this.minute = minute;
    this.second = second;
    this.nanosecond = nanosecond;
    this.offsetSeconds = offsetSeconds;
    this.zone = zone;
  }
}

/// A LOCAL_DATETIME: the wall clock, no offset or zone.
class CypherLocalDateTime {
  constructor(year, month, day, hour, minute, second, nanosecond) {
    this.year = year;
    this.month = month;
    this.day = day;
    this.hour = hour;
    this.minute = minute;
    this.second = second;
    this.nanosecond = nanosecond;
  }
}

/// A DURATION as its four lossless components — `PT36H` (seconds = 129600)
/// and `P1DT12H` (days = 1, seconds = 43200) stay distinct, where a
/// milliseconds projection would collapse them.
class CypherDuration {
  constructor(months, days, seconds, nanos) {
    this.months = months;
    this.days = days;
    this.seconds = seconds;
    this.nanos = nanos;
  }
}

/// A POINT: srid (7203/4326 2D, 9157/4979 3D) and its coordinates; `z` is
/// `undefined` on a 2D point.
class CypherPoint {
  constructor(srid, x, y, z) {
    this.srid = srid;
    this.x = x;
    this.y = y;
    this.z = z;
  }
}

/// A VECTOR: the canonical coordinate-type name (`"FLOAT32"`, `"INTEGER8"`,
/// … — one of the closed eight) and its coordinates (numbers for the FLOAT*
/// dtypes; the int64 policy for the INTEGER* dtypes).
class CypherVector {
  constructor(dtype, values) {
    this.dtype = dtype;
    this.values = values;
  }
}

/// `float(v)` — the one spelling of "bind this number as Cypher FLOAT" (a
/// documented alias of `new CypherFloat(v)`).
function float(value) {
  return new CypherFloat(value);
}

// The vector coordinate-type vocabulary: the canonical spellings, exact
// matches only (never prefixes). The INTEGER* family decides which putter
// and which coordinate reader serve a dtype; anything else is the FLOAT*
// family, whose four names the engine itself validates.
const INT_DTYPES = new Set(["INTEGER8", "INTEGER16", "INTEGER32", "INTEGER"]);

// params must be a plain object (null-prototype included): primitives,
// arrays, and Map/Set/Date/class instances would bind to the wrong shape at
// the ABI boundary. (A JS Date is deliberately NOT accepted for a datetime —
// its UTC/local reading is ambiguous; construct a CypherDateTime with
// explicit components.)
function isPlainParamsObject(v) {
  if (typeof v !== "object" || v === null) return false;
  const proto = Object.getPrototypeOf(v);
  return proto === Object.prototype || proto === null;
}

module.exports = function createValueCodec(lib) {
  // --- The value walker ---------------------------------------------------

  const value_kind = lib.func("int relate_value_kind(void *v)");
  const value_bool = lib.func("int relate_value_bool(void *v, _Out_ uint8_t *out)");
  // int64 out-params go through an 8-byte buffer read as a little-endian
  // BigInt — bypassing koffi's Number-when-safe/BigInt-when-not conversion
  // so the jsInt policy is the only conversion. Every supported platform
  // (x64/arm64, the five npm targets) is little-endian.
  const value_int = lib.func("int relate_value_int(void *v, void *out)");
  const value_float = lib.func("int relate_value_float(void *v, _Out_ double *out)");
  const value_string = lib.func("void *relate_value_string(void *v, _Out_ size_t *len)");
  const value_element_id = lib.func("str relate_value_element_id(void *v)");
  const value_start_element_id = lib.func("str relate_value_start_element_id(void *v)");
  const value_end_element_id = lib.func("str relate_value_end_element_id(void *v)");
  // Numeric entity ids (ADR-0506): same int64-out-param shape as
  // relate_value_int, so they ride the same 8-byte LE read.
  const value_id = lib.func("int relate_value_id(void *v, void *out)");
  const value_start_id = lib.func("int relate_value_start_id(void *v, void *out)");
  const value_end_id = lib.func("int relate_value_end_id(void *v, void *out)");
  const value_list_len = lib.func("int64_t relate_value_list_len(void *v)");
  const value_list_get = lib.func("void *relate_value_list_get(void *v, int64_t i)");
  const value_map_len = lib.func("int64_t relate_value_map_len(void *v)");
  const value_map_key = lib.func("void *relate_value_map_key(void *v, int64_t i, _Out_ size_t *len)");
  // The key pointer map_key returned, not a re-encoded string: a map key
  // with an interior NUL stays look-up-able by its own pointer.
  const value_map_get = lib.func("void *relate_value_map_get(void *v, void *key)");
  const value_year = lib.func("int relate_value_year(void *v, void *out)");
  const value_month = lib.func("int relate_value_month(void *v, void *out)");
  const value_day = lib.func("int relate_value_day(void *v, void *out)");
  const value_hour = lib.func("int relate_value_hour(void *v, void *out)");
  const value_minute = lib.func("int relate_value_minute(void *v, void *out)");
  const value_second = lib.func("int relate_value_second(void *v, void *out)");
  const value_nanosecond = lib.func("int relate_value_nanosecond(void *v, void *out)");
  const value_offset_seconds = lib.func("int relate_value_offset_seconds(void *v, void *out)");
  const value_zone = lib.func("str relate_value_zone(void *v)");
  const value_duration = lib.func(
    "int relate_value_duration(void *v, void *months, void *days, void *seconds, void *nanos)"
  );
  const value_point = lib.func(
    "int relate_value_point(void *v, _Out_ int32_t *srid, _Out_ int32_t *dim, _Out_ double *x, _Out_ double *y, _Out_ double *z)"
  );
  const value_vector_dtype = lib.func("str relate_value_vector_dtype(void *v)");
  const value_vector_dim = lib.func("int64_t relate_value_vector_dim(void *v)");
  const value_vector_float = lib.func("int relate_value_vector_float(void *v, int64_t i, _Out_ double *out)");
  const value_vector_int = lib.func("int relate_value_vector_int(void *v, int64_t i, void *out)");

  // --- The params builder -------------------------------------------------

  const params_new = lib.func("void *relate_params_new()");
  const params_free = lib.func("void relate_params_free(void *p)");
  const params_put_null = lib.func("int relate_params_put_null(void *p, str name, _Out_ void **err)");
  const params_put_bool = lib.func("int relate_params_put_bool(void *p, str name, uint8_t v, _Out_ void **err)");
  const params_put_int = lib.func("int relate_params_put_int(void *p, str name, int64_t v, _Out_ void **err)");
  const params_put_float = lib.func("int relate_params_put_float(void *p, str name, double v, _Out_ void **err)");
  const params_put_string = lib.func("int relate_params_put_string(void *p, str name, void *v, size_t len, _Out_ void **err)");
  const params_put_date = lib.func(
    "int relate_params_put_date(void *p, str name, int64_t year, int64_t month, int64_t day, _Out_ void **err)"
  );
  const params_put_time = lib.func(
    "int relate_params_put_time(void *p, str name, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, int64_t offset_seconds, str zone, _Out_ void **err)"
  );
  const params_put_local_time = lib.func(
    "int relate_params_put_local_time(void *p, str name, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, _Out_ void **err)"
  );
  const params_put_datetime = lib.func(
    "int relate_params_put_datetime(void *p, str name, int64_t year, int64_t month, int64_t day, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, int64_t offset_seconds, str zone, _Out_ void **err)"
  );
  const params_put_local_datetime = lib.func(
    "int relate_params_put_local_datetime(void *p, str name, int64_t year, int64_t month, int64_t day, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, _Out_ void **err)"
  );
  const params_put_duration = lib.func(
    "int relate_params_put_duration(void *p, str name, int64_t months, int64_t days, int64_t seconds, int64_t nanos, _Out_ void **err)"
  );
  const params_put_point = lib.func(
    "int relate_params_put_point(void *p, str name, int32_t srid, double x, double y, void *z, _Out_ void **err)"
  );
  const params_put_vector_float = lib.func(
    "int relate_params_put_vector_float(void *p, str name, str dtype, void *values, size_t dim, _Out_ void **err)"
  );
  const params_put_vector_int = lib.func(
    "int relate_params_put_vector_int(void *p, str name, str dtype, void *values, size_t dim, _Out_ void **err)"
  );
  const params_put_list = lib.func("int relate_params_put_list(void *p, str name, _Out_ void **child, _Out_ void **err)");
  const params_put_map = lib.func("int relate_params_put_map(void *p, str name, _Out_ void **child, _Out_ void **err)");

  // enum relate_value_kind (relate.h): the closed set this decoder
  // knows; a later, additive ABI enumerator hits the default arm's defined
  // error instead of a crash or a silent null.
  const V = {
    NULL: 0, BOOL: 1, INT: 2, FLOAT: 3, STRING: 4, NODE: 5, RELATIONSHIP: 6,
    PATH: 7, LIST: 8, MAP: 9, POINT: 10, DATE: 11, TIME: 12, LOCAL_TIME: 13,
    DATETIME: 14, LOCAL_DATETIME: 15, DURATION: 16, VECTOR: 17,
  };

  /// The decoder calls accessors only after the switch proved the kind, but
  /// a refusal is still checked — a silently zeroed component is exactly
  /// the fallback the typed ABI exists to remove. abiViolation (errors.js)
  /// is the one spelling of the prefix.
  function refused(what) {
    throw abiViolation(`walker refused an in-kind ${what} read`);
  }

  // Scratch buffer for one int64 out-param, owned by readInt64With below:
  // decode is synchronous and single-threaded (main thread, including the
  // async callback path), and the buffer is consumed before any recursion
  // can reenter the reader.
  const I64 = Buffer.alloc(8);

  /// One int64 out-param read through a caller-supplied accessor: the
  /// helper owns the shared scratch buffer — it hands the buffer to the
  /// accessor, checks the status, and decodes — so the scalar reader
  /// passes a (value, buffer) accessor and the vector reader a (value,
  /// index, buffer) one, and no call site touches the buffer itself.
  function readInt64With(read, what) {
    if (read(I64) !== 0) refused(what);
    return I64.readBigInt64LE(0);
  }

  function readInt64(fn, v, what = "int64") {
    return readInt64With((buf) => fn(v, buf), what);
  }

  function component(fn, v, what) {
    return jsInt(readInt64(fn, v, what));
  }

  /// koffi's length-aware byte read:
  /// decode exactly len bytes as a uint8 array, never to the first NUL — a
  /// Cypher string can contain `\u0000`. A zero length is the empty string
  /// (koffi's array type rejects a zero length, so it never reaches decode).
  function decodeBytes(p, len) {
    if (len === 0) return Buffer.alloc(0);
    return koffi.decode(p, koffi.array("uint8_t", len));
  }

  function readString(v) {
    const len = [0];
    const p = value_string(v, len);
    return Buffer.from(decodeBytes(p, len[0])).toString("utf8");
  }

  function decodeList(v) {
    const n = Number(value_list_len(v));
    const items = [];
    for (let i = 0; i < n; i++) items.push(decodeValue(value_list_get(v, i)));
    return items;
  }

  function decodeMap(v) {
    const n = Number(value_map_len(v));
    const entries = [];
    for (let i = 0; i < n; i++) {
      const len = [0];
      const key = value_map_key(v, i, len);
      const bytes = decodeBytes(key, len[0]);
      const name = Buffer.from(bytes).toString("utf8");
      // map_get reads its key NUL-terminated, so a key holding an interior
      // NUL would resolve to its prefix — possibly a different key's value.
      // Refuse it, as the Go and Java clients do.
      if (bytes.includes(0)) {
        throw new Error(
          `map key ${JSON.stringify(name)} holds an interior NUL and cannot be read through the walker`,
        );
      }
      entries.push([name, decodeValue(value_map_get(v, key))]);
    }
    // fromEntries defines properties, so a `__proto__` key cannot swap the
    // object's prototype.
    return Object.fromEntries(entries);
  }

  function decodePoint(v) {
    const srid = [0], dim = [0], x = [0], y = [0], z = [0];
    if (value_point(v, srid, dim, x, y, z) !== 0) refused("point");
    return dim[0] === 3 ? new CypherPoint(srid[0], x[0], y[0], z[0]) : new CypherPoint(srid[0], x[0], y[0]);
  }

  function readVectorInt(v, i) {
    return readInt64With((buf) => value_vector_int(v, i, buf), "vector coordinate");
  }

  function decodeVector(v) {
    const dtype = value_vector_dtype(v);
    const dim = Number(value_vector_dim(v));
    const values = [];
    if (INT_DTYPES.has(dtype)) {
      for (let i = 0; i < dim; i++) values.push(jsInt(readVectorInt(v, i)));
    } else {
      const out = [0];
      for (let i = 0; i < dim; i++) {
        if (value_vector_float(v, i, out) !== 0) refused("vector coordinate");
        values.push(out[0]);
      }
    }
    return new CypherVector(dtype, values);
  }

  /// One walker decode: a cell's root handle (or a list/map child) to a JS
  /// value. Kind-structural at every depth — a user map shaped like a node
  /// is a MAP, a nested path is a PATH; nothing is inferred from keys.
  function decodeValue(v) {
    const kind = value_kind(v);
    switch (kind) {
      case V.NULL:
        return null;
      case V.BOOL: {
        const out = [0];
        if (value_bool(v, out) !== 0) refused("bool");
        return out[0] !== 0;
      }
      case V.INT:
        return jsInt(readInt64(value_int, v));
      case V.FLOAT: {
        const out = [0];
        if (value_float(v, out) !== 0) refused("float");
        return out[0]; // NaN and ±Infinity arrive as themselves
      }
      case V.STRING:
        return readString(v);
      case V.NODE:
        // decodedEntity (entity.js) stamps the structural entity marker —
        // the kind argument is the walker's enumerator, never a key read
        // back from the object.
        return decodedEntity("node", {
          elementId: value_element_id(v),
          // The deprecated numeric id, exactly id(n) (ADR-0506), under the
          // same Int64 policy as every INTEGER.
          id: component(value_id, v, "entity id"),
          labels: decodeList(v), // a NODE's list is its labels
          properties: decodeMap(v), // and its map its properties
        });
      case V.RELATIONSHIP:
        return decodedEntity("relationship", {
          elementId: value_element_id(v),
          id: component(value_id, v, "entity id"),
          type: readString(v), // a RELATIONSHIP's string is its type
          startElementId: value_start_element_id(v),
          endElementId: value_end_element_id(v),
          // id(startNode(r)) / id(endNode(r)), same Int64 policy.
          startId: component(value_start_id, v, "entity start id"),
          endId: component(value_end_id, v, "entity end id"),
          properties: decodeMap(v),
        });
      case V.PATH: // node, rel, node, …, node — the elements carry their kinds
      case V.LIST:
        return decodeList(v);
      case V.MAP:
        return decodeMap(v);
      case V.POINT:
        return decodePoint(v);
      case V.DATE:
        return new CypherDate(
          component(value_year, v), component(value_month, v), component(value_day, v)
        );
      case V.TIME:
        return new CypherTime(
          component(value_hour, v), component(value_minute, v),
          component(value_second, v), component(value_nanosecond, v),
          component(value_offset_seconds, v)
        );
      case V.LOCAL_TIME:
        return new CypherLocalTime(
          component(value_hour, v), component(value_minute, v),
          component(value_second, v), component(value_nanosecond, v)
        );
      case V.DATETIME: {
        const zone = value_zone(v); // "" when the value carries only an offset
        return new CypherDateTime(
          component(value_year, v), component(value_month, v), component(value_day, v),
          component(value_hour, v), component(value_minute, v),
          component(value_second, v), component(value_nanosecond, v),
          component(value_offset_seconds, v), zone === "" ? null : zone
        );
      }
      case V.LOCAL_DATETIME:
        return new CypherLocalDateTime(
          component(value_year, v), component(value_month, v), component(value_day, v),
          component(value_hour, v), component(value_minute, v),
          component(value_second, v), component(value_nanosecond, v)
        );
      case V.DURATION: {
        const months = Buffer.alloc(8), days = Buffer.alloc(8), seconds = Buffer.alloc(8), nanos = Buffer.alloc(8);
        if (value_duration(v, months, days, seconds, nanos) !== 0) refused("duration");
        return new CypherDuration(
          jsInt(months.readBigInt64LE(0)), jsInt(days.readBigInt64LE(0)),
          jsInt(seconds.readBigInt64LE(0)), jsInt(nanos.readBigInt64LE(0))
        );
      }
      case V.VECTOR:
        return decodeVector(v);
      default:
        // A later, additive ABI: defined and reportable, never a crash or a
        // silent null.
        throw new Error(`unsupported value kind ${kind} (a newer librelate_ffi?)`);
    }
  }

  /// Run one putter with its own error out-array. A refusal (duplicate
  /// name, out-of-range component, unknown zone, …) goes to `onRefusal`,
  /// which index.js turns into the thrown error from the ABI's error shape;
  /// the builder stays usable either way. One helper so every put stays one
  /// line, and the out-array is per call — never shared between puts.
  function runPut(onRefusal, putter, p, name, ...rest) {
    const err = [null];
    const rc = putter(p, name, ...rest, err);
    if (rc !== 0) onRefusal(err);
  }

  /// Pack a vector's coordinates into the buffer the putter reads: doubles
  /// for the FLOAT* family, little-endian int64s for the INTEGER* family.
  function vectorValuesBuffer(vector, label) {
    const dim = vector.values.length;
    const buf = Buffer.alloc(8 * dim);
    if (INT_DTYPES.has(vector.dtype)) {
      for (let i = 0; i < dim; i++) {
        const x = vector.values[i];
        if (typeof x !== "bigint" && !Number.isInteger(x)) {
          throw new TypeError(`${label} coordinate ${i} must be an integer for dtype ${vector.dtype}`);
        }
        buf.writeBigInt64LE(asInt64(BigInt(x), `${label} coordinate ${i}`), i * 8);
      }
      return { buf, dim, int: true };
    }
    for (let i = 0; i < dim; i++) {
      const x = vector.values[i];
      if (typeof x !== "number") {
        throw new TypeError(`${label} coordinate ${i} must be a number for dtype ${vector.dtype}`);
      }
      buf.writeDoubleLE(x, i * 8);
    }
    return { buf, dim, int: false };
  }

  function pointZBuffer(z) {
    const buf = Buffer.alloc(8);
    buf.writeDoubleLE(z, 0);
    return buf;
  }

  // One table drives every typed class's binding: the class, its putter,
  // and its argument tuple — the same runPut shape for every entry, so a
  // class is a row here, not another instanceof arm in putValue. `put`
  // maps the value to [putter, rest-arguments]; the vector row takes the
  // label for its coordinate TypeErrors.
  const TYPED_PUTS = [
    [CypherFloat, (v) => [params_put_float, [v.value]]],
    [CypherDate, (v) => [params_put_date, [v.year, v.month, v.day]]],
    [CypherLocalTime, (v) => [params_put_local_time, [v.hour, v.minute, v.second, v.nanosecond]]],
    // A TIME keeps no zone id; binding one is offset-only.
    [CypherTime, (v) => [params_put_time, [v.hour, v.minute, v.second, v.nanosecond, v.offsetSeconds, null]]],
    [CypherLocalDateTime, (v) => [
      params_put_local_datetime,
      [v.year, v.month, v.day, v.hour, v.minute, v.second, v.nanosecond],
    ]],
    // zone null: offset-only; a zone id keeps the zone and re-resolves the
    // offset at that wall clock.
    [CypherDateTime, (v) => [
      params_put_datetime,
      [v.year, v.month, v.day, v.hour, v.minute, v.second, v.nanosecond, v.offsetSeconds, v.zone],
    ]],
    [CypherDuration, (v) => [params_put_duration, [v.months, v.days, v.seconds, v.nanos]]],
    // z is a pointer to the third coordinate (3D) or NULL (2D).
    [CypherPoint, (v) => [params_put_point, [v.srid, v.x, v.y, v.z === undefined ? null : pointZBuffer(v.z)]]],
    [CypherVector, (v, label) => {
      const { buf, dim, int } = vectorValuesBuffer(v, label);
      return [int ? params_put_vector_int : params_put_vector_float, [v.dtype, buf, dim]];
    }],
  ];

  /// Bind one typed value class instance, if `v` is one: the table row's
  /// putter and argument tuple through the one runPut. Returns whether it
  /// was — putValue's remaining object arms (list, plain map) run only
  /// when it was not.
  function putTypedValue(p, name, v, label, onRefusal) {
    for (const [cls, put] of TYPED_PUTS) {
      if (v instanceof cls) {
        const [putter, args] = put(v, label);
        runPut(onRefusal, putter, p, name, ...args);
        return true;
      }
    }
    return false;
  }

  /// Bind one JS value under `name` (null for a list slot) into builder `p`.
  /// The deep value policy lives here — the binding IS the validation:
  /// every leaf that cannot bind rejects with a TypeError naming its path
  /// from the params root, instead of being silently dropped. `seen` holds
  /// the containers on the current path from the root (entered on the way
  /// down, removed on the way out): a container that contains itself is a
  /// cycle and refuses, while the same object reached twice by different
  /// paths — a shared reference — binds each time.
  function putValue(p, name, v, label, onRefusal, seen) {
    if (v === null) return runPut(onRefusal, params_put_null, p, name);
    switch (typeof v) {
      case "boolean":
        return runPut(onRefusal, params_put_bool, p, name, v ? 1 : 0);
      case "string": {
        // Buffer, not str marshalling: the byte length crosses with it, so
        // an interior NUL survives.
        const b = Buffer.from(v, "utf8");
        return runPut(onRefusal, params_put_string, p, name, b, b.byteLength);
      }
      case "bigint":
        // Full-range, exact: a BigInt crosses to int64_t untouched.
        return runPut(onRefusal, params_put_int, p, name, asInt64(v, label));
      case "number":
        if (Number.isInteger(v)) {
          // Integer-valued numbers bind as INTEGER. Beyond ±2^53-1 a Number
          // can already be imprecise — pass a BigInt for exactness.
          return runPut(onRefusal, params_put_int, p, name, asInt64(BigInt(v), label));
        }
        // Non-finite included: NaN and ±Infinity bind (the JSON era could
        // not carry them; ADR-0502's builder can).
        return runPut(onRefusal, params_put_float, p, name, v);
      case "object":
        break;
      default:
        // undefined, functions, symbols: no Cypher representation.
        throw new TypeError(`${label} cannot be a Cypher parameter`);
    }
    if (Array.isArray(v)) {
      const child = [null];
      runPut(onRefusal, params_put_list, p, name, child);
      // A LIST takes NULL names and appends in call order.
      return walkContainer(v, label, seen, () => {
        for (let i = 0; i < v.length; i++) {
          putValue(child[0], null, v[i], `${label}[${i}]`, onRefusal, seen);
        }
      });
    }
    if (putTypedValue(p, name, v, label, onRefusal)) return;
    if (!isPlainParamsObject(v)) {
      throw new TypeError(`${label} must be a plain object`);
    }
    const child = [null];
    runPut(onRefusal, params_put_map, p, name, child);
    return walkContainer(v, label, seen, () => {
      for (const key of Object.keys(v)) {
        putValue(child[0], key, v[key], `${label}.${key}`, onRefusal, seen);
      }
    });
  }

  /// Walk one LIST/MAP container's children with `v` on the current path:
  /// refuse if it is already there (a true cycle), and take it off again on
  /// the way out — success or throw — so a shared, acyclic reference
  /// elsewhere in the tree is not mistaken for a cycle.
  function walkContainer(v, label, seen, walkChildren) {
    if (seen.has(v)) throw new TypeError(`${label} is cyclic`);
    seen.add(v);
    try {
      walkChildren();
    } finally {
      seen.delete(v);
    }
  }

  /// Build a params root from a JS params object; null for the empty object
  /// (the ABI's no-params form — the JSON era's "{}" is gone).
  function buildParams(params, onRefusal) {
    const keys = Object.keys(params);
    if (keys.length === 0) return null;
    const root = params_new();
    try {
      const seen = new Set();
      for (const key of keys) {
        putValue(root, key, params[key], `query params: "${key}"`, onRefusal, seen);
      }
    } catch (e) {
      params_free(root);
      throw e;
    }
    return root;
  }

  /// Free a params builder, the null no-params root included. The free
  /// policy — like the build policy — is the codec's, so every caller
  /// frees unconditionally instead of re-encoding the null case at each
  /// call site.
  function paramsFree(root) {
    if (root !== null) params_free(root);
  }

  return {
    decodeValue,
    buildParams,
    paramsFree,
    isPlainParamsObject,
    CypherFloat,
    CypherDate,
    CypherTime,
    CypherLocalTime,
    CypherDateTime,
    CypherLocalDateTime,
    CypherDuration,
    CypherPoint,
    CypherVector,
    float,
  };
};
