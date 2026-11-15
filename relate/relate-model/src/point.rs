//! The point axis vocabulary: the two spellings of each coordinate
//! (`x`/`longitude`, `y`/`latitude`, `z`/`height`) and the geographic-SRID
//! predicate. One home for the alias fact, consulted by the `point()` input
//! readers, the component accessors and the wire encoder.

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

/// Whether an SRID is geographic (longitude/latitude keys, `wgs-84*` CRS).
pub fn is_geographic(srid: i32) -> bool {
    matches!(srid, 4326 | 4979)
}

/// Neo4j's longitude wrap into `[-180, 180]`, keeping both endpoints
/// (180.0 → 180.0, -180.0 → -180.0, 180.5 → -179.5, -200.0 → 160.0,
/// -540.0 → -180.0). This is the stored geographic-longitude domain for
/// the producers that uphold it — the engine's `point()` constructor and
/// the Query API typed decoders — so the POINT index's envelope clip
/// (which assumes the domain) and the residual scan cannot disagree
/// (LABS-1236). Bolt point parameters are stored bit-for-bit instead, by
/// ADR-0177 §2.
pub fn wrap_longitude(longitude: f64) -> f64 {
    let mut x = longitude;
    if !(-180.0..=180.0).contains(&x) {
        x %= 360.0;
        if x > 180.0 {
            x -= 360.0;
        } else if x < -180.0 {
            x += 360.0;
        }
    }
    // Normalize -0.0 (from -360 % 360) to +0.0.
    if x == 0.0 {
        0.0
    } else {
        x
    }
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
