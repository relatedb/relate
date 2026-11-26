//! The point axis vocabulary: the two spellings of each coordinate
//! (`x`/`longitude`, `y`/`latitude`, `z`/`height`) and the geographic-SRID
//! predicate. One home for the alias fact, consulted by the `point()` input
//! readers, the component accessors and the wire encoder.

use std::fmt;

/// One point axis with both of its spellings: the cartesian name and the
/// geographic name denote the same coordinate (`x` is `longitude`). The one
/// alias vocabulary the `point()` input readers, the component accessors and
/// the wire encoder consult, so the pairs are never spelled at a fourth site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointAxis {
    /// The axis position: 0 = x/longitude, 1 = y/latitude, 2 = z/height.
    pub index: usize,
    /// The cartesian spelling.
    pub cartesian: &'static str,
    /// The geographic spelling.
    pub geographic: &'static str,
}

impl PointAxis {
    /// Whether `key` is either spelling of this axis.
    pub fn names(self, key: &str) -> bool {
        key == self.cartesian || key == self.geographic
    }

    /// The spelling this axis uses on a point of the given family.
    pub fn key(self, geographic: bool) -> &'static str {
        if geographic {
            self.geographic
        } else {
            self.cartesian
        }
    }
}

/// The three point axes in x/y/z order.
pub const POINT_AXES: [PointAxis; 3] = [
    PointAxis {
        index: 0,
        cartesian: "x",
        geographic: "longitude",
    },
    PointAxis {
        index: 1,
        cartesian: "y",
        geographic: "latitude",
    },
    PointAxis {
        index: 2,
        cartesian: "z",
        geographic: "height",
    },
];

/// The axis a coordinate key names, or `None` for a non-coordinate key.
pub fn point_axis(key: &str) -> Option<PointAxis> {
    POINT_AXES.into_iter().find(|axis| axis.names(key))
}

/// The three coordinate key spellings a point of the given family uses.
pub fn point_axis_keys(geographic: bool) -> [&'static str; 3] {
    [
        POINT_AXES[0].key(geographic),
        POINT_AXES[1].key(geographic),
        POINT_AXES[2].key(geographic),
    ]
}

/// The four `point()` SRIDs' fixed facts — their dimension (2 or 3) and
/// whether they are geographic — one row each. `None` for any other SRID (a
/// wire decoder can carry one; `point()` cannot).
///
/// This is the one dimension/axis-family table: [`is_geographic`],
/// [`srid_dimension`], `PointValue::crs_name`'s dimensionality check and the
/// executor's `Srid` family/dimension all derive from it, so a fifth code or
/// a corrected dimension lands in exactly one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SridFacts {
    dimension: usize,
    geographic: bool,
}

fn srid_facts(srid: i32) -> Option<SridFacts> {
    let (dimension, geographic) = match srid {
        7203 => (2, false),
        4326 => (2, true),
        9157 => (3, false),
        4979 => (3, true),
        _ => return None,
    };
    Some(SridFacts {
        dimension,
        geographic,
    })
}

/// Whether an SRID is geographic (longitude/latitude keys, `wgs-84*` CRS).
pub fn is_geographic(srid: i32) -> bool {
    srid_facts(srid).is_some_and(|facts| facts.geographic)
}

/// The dimension one of the four `point()` SRIDs fixes (`2` or `3`), or
/// `None` for any other SRID.
pub fn srid_dimension(srid: i32) -> Option<usize> {
    srid_facts(srid).map(|facts| facts.dimension)
}

/// The stored geographic-longitude domain's lower bound, as `point()`
/// validates it — the one home the bounds and one turn derive from.
pub const LONGITUDE_MIN: f64 = -180.0;
/// The stored geographic-longitude domain's upper bound.
pub const LONGITUDE_MAX: f64 = 180.0;
/// One full turn of [`LONGITUDE_MIN`]..[`LONGITUDE_MAX`] — the shift a
/// wrapping band takes into the unwrapped frame.
pub const LONGITUDE_TURN: f64 = LONGITUDE_MAX - LONGITUDE_MIN;
/// The stored geographic-latitude domain's lower bound, as `point()`
/// validates it.
pub const LATITUDE_MIN: f64 = -90.0;
/// The stored geographic-latitude domain's upper bound.
pub const LATITUDE_MAX: f64 = 90.0;

/// Neo4j's longitude wrap into `[LONGITUDE_MIN, LONGITUDE_MAX]`, keeping
/// both endpoints (180.0 → 180.0, -180.0 → -180.0, 180.5 → -179.5,
/// -200.0 → 160.0, -540.0 → -180.0). This is the stored
/// geographic-longitude domain for the producers that uphold it — the
/// engine's `point()` constructor and the Query API typed decoders — so the
/// POINT index's envelope clip (which assumes the domain) and the residual
/// scan cannot disagree (LABS-1236). Bolt point parameters are stored
/// bit-for-bit instead, by ADR-0177 §2.
pub fn wrap_longitude(longitude: f64) -> f64 {
    // One pipeline over the domain constants: the remainder lands in
    // (-360, 360), and the endpoint ladder folds the two half-turns
    // outside the domain back onto it. In-domain values take the
    // remainder untouched (fmod keeps them bit-identical).
    let x = longitude % LONGITUDE_TURN;
    let x = if x > LONGITUDE_MAX {
        x - LONGITUDE_TURN
    } else if x < LONGITUDE_MIN {
        x + LONGITUDE_TURN
    } else {
        x
    };
    // Normalize -0.0 (from -360 % 360) to +0.0.
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

/// Why a coordinate triple is not a storable point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointDomainError {
    /// A geographic point's latitude is outside `[-90, 90]`.
    InvalidLatitude,
    /// A coordinate is not finite.
    NonFiniteCoordinate,
    /// The coordinate count disagrees with the SRID's fixed dimension.
    InvalidDimension { srid: i32 },
}

/// Why an EWKT string was refused: the text does not match the grammar, or
/// it does and the coordinates are outside the stored domain. One typed
/// error so a caller can tell the two halves apart; `Display` is the text
/// every surface already shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EwktError {
    /// The text does not match the EWKT grammar.
    Grammar(String),
    /// The coordinates are outside the stored domain.
    Domain(PointDomainError),
}

impl fmt::Display for PointDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PointDomainError::InvalidLatitude => {
                f.write_str("latitude must be in the range [-90, 90]")
            }
            PointDomainError::NonFiniteCoordinate => f.write_str("a coordinate is not finite"),
            PointDomainError::InvalidDimension { srid } => {
                write!(f, "SRID={srid} does not match the coordinate dimension")
            }
        }
    }
}

impl std::error::Error for PointDomainError {}

impl fmt::Display for EwktError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EwktError::Grammar(detail) => f.write_str(detail),
            EwktError::Domain(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for EwktError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EwktError::Grammar(_) => None,
            EwktError::Domain(err) => Some(err),
        }
    }
}

/// The one stored-domain rule every point producer shares (the `point()`
/// constructor and every wire decoder): a geographic point's longitude wraps
/// into `[-180, 180]` by [`wrap_longitude`] and its latitude must already be
/// in `[-90, 90]`; a cartesian point is stored as given. Keeping it here is
/// what stops a decoder admitting a point the constructor refuses.
///
/// The **SRID** is the one discriminator, never the coordinate-key family:
/// a `point({x, y, srid: 4326})` map and a decoded `SRID=4326` WKT are the
/// same value, so they must pass the same rule (Neo4j validates by SRID too).
/// The key family only decides which keys the map reader accepts. The whole
/// domain lives here — finiteness, the SRID's dimension, and the geographic
/// longitude/latitude rule — so every producer inherits it.
pub fn stored_point(
    srid: i32,
    x: f64,
    y: f64,
    z: Option<f64>,
) -> Result<crate::value::PointValue, PointDomainError> {
    if !x.is_finite() || !y.is_finite() || z.is_some_and(|z| !z.is_finite()) {
        return Err(PointDomainError::NonFiniteCoordinate);
    }
    if let Some(dimension) = srid_dimension(srid) {
        let actual = if z.is_some() { 3 } else { 2 };
        if actual != dimension {
            return Err(PointDomainError::InvalidDimension { srid });
        }
    }
    if is_geographic(srid) {
        if !(LATITUDE_MIN..=LATITUDE_MAX).contains(&y) {
            return Err(PointDomainError::InvalidLatitude);
        }
        Ok(crate::value::PointValue {
            srid,
            x: wrap_longitude(x),
            y,
            z,
        })
    } else {
        Ok(crate::value::PointValue { srid, x, y, z })
    }
}

/// The Query API's EWKT spelling of a point (ADR-0181 item 5) —
/// `SRID=7203;POINT (1.0 2.0)` or `SRID=4979;POINT Z (12.5 55.7 10.0)`.
/// Beside [`parse_ewkt`], the one grammar's two halves.
pub fn ewkt(p: &crate::value::PointValue) -> String {
    let text = crate::value::float_text;
    match p.z {
        Some(z) => format!(
            "SRID={};POINT Z ({} {} {})",
            p.srid,
            text(p.x),
            text(p.y),
            text(z)
        ),
        None => format!("SRID={};POINT ({} {})", p.srid, text(p.x), text(p.y)),
    }
}

/// Parse the Query API's EWKT point text into a stored point (ADR-0181
/// item 5): `SRID=<i32>;POINT (<x> <y>)` or `SRID=<i32>;POINT Z (<x> <y>
/// <z>)`, anything else refused. One grammar for every Query API surface
/// that reads a point back (the typed wire decoder and the HTTP request
/// fold), so the accepted shapes cannot drift; the stored domain is
/// [`stored_point`]'s rule and coordinates must be finite.
pub fn parse_ewkt(text: &str) -> Result<crate::value::PointValue, EwktError> {
    let (srid, rest) = text
        .strip_prefix("SRID=")
        .and_then(|rest| rest.split_once(';'))
        .ok_or_else(|| EwktError::Grammar(format!("{text:?} is not \"SRID=<i32>;POINT (…)\"")))?;
    let srid: i32 = srid
        .parse()
        .map_err(|_| EwktError::Grammar(format!("{srid:?} is not an i32 SRID")))?;
    let (three_d, inner) = if let Some(inner) = rest.strip_prefix("POINT Z (") {
        (true, inner)
    } else if let Some(inner) = rest.strip_prefix("POINT (") {
        (false, inner)
    } else {
        return Err(EwktError::Grammar(format!(
            "{rest:?} is not \"POINT (…)\" or \"POINT Z (…)\""
        )));
    };
    let inner = inner
        .strip_suffix(')')
        .ok_or_else(|| EwktError::Grammar("missing the closing \")\"".to_string()))?;
    let coords: Vec<&str> = inner.split_whitespace().collect();
    let expected = if three_d { 3 } else { 2 };
    if coords.len() != expected {
        return Err(EwktError::Grammar(format!(
            "expected {expected} coordinates, found {}",
            coords.len()
        )));
    }
    let x = coordinate(coords[0])?;
    let y = coordinate(coords[1])?;
    let z = coords.get(2).map(|z| coordinate(z)).transpose()?;
    stored_point(srid, x, y, z).map_err(EwktError::Domain)
}

/// One point coordinate: a finite decimal (`NaN`/`Infinity` parse as
/// floats but no point envelope contains them).
fn coordinate(token: &str) -> Result<f64, EwktError> {
    let value: f64 = token
        .parse()
        .map_err(|_| EwktError::Grammar(format!("{token:?} is not a decimal coordinate")))?;
    if !value.is_finite() {
        return Err(EwktError::Grammar(format!(
            "{token:?} is not a finite coordinate"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longitude_wraps_into_the_stored_domain() {
        for (longitude, wrapped) in [
            (0.0, 0.0),
            (180.0, 180.0),
            (-180.0, -180.0),
            (180.5, -179.5),
            (-200.0, 160.0),
            (370.0, 10.0),
            (-540.0, -180.0),
            (540.0, 180.0),
        ] {
            assert_eq!(wrap_longitude(longitude), wrapped, "{longitude}");
        }
        // -360 % 360 is -0.0; the wrap normalizes it to +0.0.
        assert!(wrap_longitude(-360.0).is_sign_positive());
        // Extreme longitudes stay finite and in range.
        for longitude in [1e30, -1e30, 1e300, -1e300, f64::MAX, -f64::MAX] {
            let wrapped = wrap_longitude(longitude);
            assert!(
                wrapped.is_finite() && (-180.0..=180.0).contains(&wrapped),
                "longitude {longitude} wrapped to {wrapped}"
            );
        }
    }
}
