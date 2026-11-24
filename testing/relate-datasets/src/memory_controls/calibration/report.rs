//! The calibration report model and its statistics: the observation, summary
//! and report records, and the fits and spreads that summarize a matrix of
//! observations. The measurement protocol stays in the parent module.

use serde::Serialize;

use super::{FamilySpec, FAMILIES, SHAPES};

/// The one logical-byte total the equal-total shapes share. A shape is
/// equal-total when its target times its holder count is this total; the
/// rows are derived from [`SHAPES`] by that rule, never copied out of it.
const EQUAL_TOTAL_LOGICAL_BYTES: u64 = 16_777_216;

/// The equal-total subset of [`SHAPES`]: the same total at one, two, and
/// four holders.
fn equal_total_shapes() -> impl Iterator<Item = (u64, usize)> {
    SHAPES
        .iter()
        .copied()
        .filter(|&(target, holders)| target * holders as u64 == EQUAL_TOTAL_LOGICAL_BYTES)
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationObservation {
    pub family: String,
    /// Logical bytes the shape asked the hold to reach.
    pub target_logical_bytes: u64,
    pub holders: usize,
    pub repetition: u32,
    /// Logical bytes reserved while the hold was sampled, summed over holders.
    pub held_logical_bytes: u64,
    pub parked_rss_bytes: u64,
    pub held_rss_bytes: u64,
    pub rss_delta_bytes: u64,
    pub rss_bytes_per_logical_byte: u64,
    pub rss_to_logical_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parked_cgroup_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_cgroup_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_delta_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_bytes_per_logical_byte: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_to_logical_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationSummary {
    pub family: String,
    pub observations: usize,
    pub rss_slope: f64,
    pub rss_intercept_bytes: f64,
    pub rss_r_squared: f64,
    pub rss_max_abs_residual_bytes: f64,
    pub max_rss_to_logical_ratio: f64,
    pub rss_repeat_spread: f64,
    pub rss_equal_total_concurrency_spread: f64,
    /// Slope over the one-holder shapes only: RSS per logical byte with no
    /// concurrency term mixed in.
    pub rss_single_holder_slope: f64,
    pub rss_single_holder_r_squared: f64,
    /// Fitted RSS bytes each extra concurrent query adds at a constant
    /// total of logical bytes. `None` for `overall`, which mixes families.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_per_holder_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_slope: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_single_holder_slope: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_single_holder_r_squared: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_per_holder_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_intercept_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_r_squared: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_max_abs_residual_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cgroup_to_logical_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_repeat_spread: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_equal_total_concurrency_spread: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationReport {
    pub engine_bytes_per_logical_byte: u64,
    pub repetitions: u32,
    pub observations: Vec<PeakCalibrationObservation>,
    pub summaries: Vec<PeakCalibrationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conservative_cgroup_ratio: Option<f64>,
}

/// What a summary covers: one family's observations, or every observation
/// for the overall row.
#[derive(Clone, Copy)]
pub(super) enum Scope {
    Overall,
    Family(&'static FamilySpec),
}

impl Scope {
    /// The wire name: the family's, or `overall`.
    fn name(self) -> &'static str {
        match self {
            Self::Overall => "overall",
            Self::Family(spec) => spec.name,
        }
    }

    fn covers(self, row: &PeakCalibrationObservation) -> bool {
        match self {
            Self::Overall => true,
            Self::Family(spec) => row.family == spec.name,
        }
    }

    /// Bytes per extra holder for this scope's rows: a family's holder
    /// cost from the equal-total shapes, or `None` for `overall`, which
    /// mixes families so a holder term is not meaningful. The one home of
    /// the per-family rule. Each delta is rescaled to 16,777,216 logical
    /// bytes first, so a few bytes of hold noise are not read as holder
    /// cost.
    fn per_holder_bytes(
        self,
        rows: &[&PeakCalibrationObservation],
        delta: impl Fn(&PeakCalibrationObservation) -> Option<u64>,
    ) -> Option<f64> {
        let Self::Family(_) = self else {
            return None;
        };
        let points = rows
            .iter()
            .filter(|row| {
                equal_total_shapes().any(|shape| shape == (row.target_logical_bytes, row.holders))
            })
            .filter_map(|row| {
                let bytes = delta(row)? as f64;
                let scale = EQUAL_TOTAL_LOGICAL_BYTES as f64 / row.held_logical_bytes.max(1) as f64;
                Some((row.holders as f64, bytes * scale))
            })
            .collect::<Vec<_>>();
        let holder_counts = points
            .iter()
            .map(|point| point.0 as usize)
            .collect::<std::collections::BTreeSet<_>>();
        (holder_counts.len() >= 2)
            .then(|| linear_fit(&points).map(|fit| fit.slope))
            .flatten()
    }
}

/// Summarize one scope's observations. The summary's `family` field
/// carries the scope's wire name; no caller discriminates a magic string. A
/// scope with no observations, or with no one-holder observation to fit the single-holder slope on, is an error:
/// there is no honest fit to report.
pub(super) fn summarize_family(
    scope: Scope,
    observations: &[PeakCalibrationObservation],
) -> Result<PeakCalibrationSummary, String> {
    let name = scope.name();
    let rows = observations
        .iter()
        .filter(|row| scope.covers(row))
        .collect::<Vec<_>>();
    let rss_points = logical_byte_points(&rows, |row| Some(row.rss_delta_bytes));
    let cgroup_points = logical_byte_points(&rows, |row| row.cgroup_delta_bytes);
    let rss_fit = linear_fit(&rss_points)
        .ok_or_else(|| format!("calibration scope {name} has no observations"))?;
    let cgroup_fit = linear_fit(&cgroup_points);
    let rss_max_abs_residual_bytes = max_abs_residual(&rss_points, rss_fit);
    let cgroup_max_abs_residual_bytes = cgroup_fit.map(|fit| max_abs_residual(&cgroup_points, fit));
    let single = rows
        .iter()
        .copied()
        .filter(|row| row.holders == 1)
        .collect::<Vec<_>>();
    let rss_single = linear_fit(&logical_byte_points(&single, |row| {
        Some(row.rss_delta_bytes)
    }))
    .ok_or_else(|| format!("calibration scope {name} has no one-holder observations"))?;
    let cgroup_single_points = logical_byte_points(&single, |row| row.cgroup_delta_bytes);
    let cgroup_single = linear_fit(&cgroup_single_points);
    Ok(PeakCalibrationSummary {
        family: name.to_string(),
        observations: rows.len(),
        rss_slope: rss_fit.slope,
        rss_intercept_bytes: rss_fit.intercept,
        rss_r_squared: rss_fit.r_squared,
        rss_max_abs_residual_bytes,
        max_rss_to_logical_ratio: rows
            .iter()
            .map(|row| row.rss_to_logical_ratio)
            .fold(0.0, f64::max),
        rss_repeat_spread: repeat_spread(&rows, |row| Some(row.rss_to_logical_ratio))
            .unwrap_or(0.0),
        rss_equal_total_concurrency_spread: equal_total_spread(&rows, |row| {
            Some(row.rss_delta_bytes as f64)
        })
        .unwrap_or(0.0),
        rss_single_holder_slope: rss_single.slope,
        rss_single_holder_r_squared: rss_single.r_squared,
        rss_per_holder_bytes: scope.per_holder_bytes(&rows, |row| Some(row.rss_delta_bytes)),
        cgroup_slope: cgroup_fit.map(|fit| fit.slope),
        cgroup_single_holder_slope: cgroup_single.map(|fit| fit.slope),
        cgroup_single_holder_r_squared: cgroup_single.map(|fit| fit.r_squared),
        cgroup_per_holder_bytes: scope.per_holder_bytes(&rows, |row| row.cgroup_delta_bytes),
        cgroup_intercept_bytes: cgroup_fit.map(|fit| fit.intercept),
        cgroup_r_squared: cgroup_fit.map(|fit| fit.r_squared),
        cgroup_max_abs_residual_bytes,
        max_cgroup_to_logical_ratio: rows
            .iter()
            .filter_map(|row| row.cgroup_to_logical_ratio)
            .reduce(f64::max),
        cgroup_repeat_spread: repeat_spread(&rows, |row| row.cgroup_to_logical_ratio),
        cgroup_equal_total_concurrency_spread: equal_total_spread(&rows, |row| {
            row.cgroup_delta_bytes.map(|bytes| bytes as f64)
        }),
    })
}

fn logical_byte_points(
    rows: &[&PeakCalibrationObservation],
    delta: impl Fn(&PeakCalibrationObservation) -> Option<u64>,
) -> Vec<(f64, f64)> {
    rows.iter()
        .filter_map(|row| delta(row).map(|bytes| (row.held_logical_bytes as f64, bytes as f64)))
        .collect()
}

fn max_abs_residual(points: &[(f64, f64)], fit: Fit) -> f64 {
    points
        .iter()
        .map(|point| (point.1 - (fit.intercept + fit.slope * point.0)).abs())
        .fold(0.0, f64::max)
}

/// The observations of one shape — one family at one target and holder
/// count — mapped through `value`. The one shape selector; both spreads
/// and any future per-shape statistic go through it.
fn shape_values(
    rows: &[&PeakCalibrationObservation],
    family: &str,
    target: u64,
    holders: usize,
    value: &impl Fn(&PeakCalibrationObservation) -> Option<f64>,
) -> Vec<f64> {
    rows.iter()
        .filter(|row| {
            row.family == family && row.target_logical_bytes == target && row.holders == holders
        })
        .filter_map(|row| value(row))
        .collect()
}

fn repeat_spread(
    rows: &[&PeakCalibrationObservation],
    value: impl Fn(&PeakCalibrationObservation) -> Option<f64>,
) -> Option<f64> {
    let mut largest = None::<f64>;
    for spec in FAMILIES {
        for &(target, holders) in SHAPES {
            let values = shape_values(rows, spec.name, target, holders, &value);
            if values.len() > 1 {
                let (min, max) = min_max(&values);
                largest = Some(largest.unwrap_or(0.0).max(max - min));
            }
        }
    }
    largest
}

fn equal_total_spread(
    rows: &[&PeakCalibrationObservation],
    value: impl Fn(&PeakCalibrationObservation) -> Option<f64>,
) -> Option<f64> {
    let mut largest = None::<f64>;
    for spec in FAMILIES {
        let means = equal_total_shapes()
            .filter_map(|(target, holders)| {
                let values = shape_values(rows, spec.name, target, holders, &value);
                (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
            })
            .collect::<Vec<_>>();
        if means.len() == equal_total_shapes().count() {
            let (min, max) = min_max(&means);
            if min > 0.0 {
                largest = Some(largest.unwrap_or(0.0).max(max / min - 1.0));
            }
        }
    }
    largest
}

/// The smallest and largest of `values` (infinities for an empty slice;
/// both callers hold at least one value).
fn min_max(values: &[f64]) -> (f64, f64) {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (min, max)
}

/// A least-squares line through (logical bytes, delta bytes) points.
#[derive(Debug, Clone, Copy)]
struct Fit {
    slope: f64,
    intercept: f64,
    r_squared: f64,
}

/// The fit through `points`; `None` for no points (there is no line to
/// report, and a zero fit would read as a measured flat one).
fn linear_fit(points: &[(f64, f64)]) -> Option<Fit> {
    if points.is_empty() {
        return None;
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|point| point.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|point| point.1).sum::<f64>() / n;
    let sxx = points
        .iter()
        .map(|point| (point.0 - mean_x).powi(2))
        .sum::<f64>();
    let sxy = points
        .iter()
        .map(|point| (point.0 - mean_x) * (point.1 - mean_y))
        .sum::<f64>();
    let slope = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    let intercept = mean_y - slope * mean_x;
    let residual = points
        .iter()
        .map(|point| (point.1 - (intercept + slope * point.0)).powi(2))
        .sum::<f64>();
    let total = points
        .iter()
        .map(|point| (point.1 - mean_y).powi(2))
        .sum::<f64>();
    let r_squared = if total == 0.0 {
        1.0
    } else {
        1.0 - residual / total
    };
    Some(Fit {
        slope,
        intercept,
        r_squared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_controls::calibration::family_spec;

    #[test]
    fn linear_fit_recovers_slope_and_intercept() {
        let Fit {
            slope,
            intercept,
            r_squared,
        } = linear_fit(&[(1.0, 7.0), (2.0, 9.0), (3.0, 11.0)]).expect("a fit");
        assert!((slope - 2.0).abs() < f64::EPSILON);
        assert!((intercept - 5.0).abs() < f64::EPSILON);
        assert!((r_squared - 1.0).abs() < f64::EPSILON);
    }

    fn observation(target: u64, holders: usize, held: u64, rss: u64) -> PeakCalibrationObservation {
        PeakCalibrationObservation {
            family: "create".into(),
            target_logical_bytes: target,
            holders,
            repetition: 0,
            held_logical_bytes: held,
            parked_rss_bytes: 0,
            held_rss_bytes: rss,
            rss_delta_bytes: rss,
            rss_bytes_per_logical_byte: rss / held.max(1),
            rss_to_logical_ratio: rss as f64 / held.max(1) as f64,
            parked_cgroup_bytes: None,
            held_cgroup_bytes: None,
            cgroup_delta_bytes: None,
            cgroup_bytes_per_logical_byte: None,
            cgroup_to_logical_ratio: None,
        }
    }

    #[test]
    fn summary_separates_single_query_slope_from_holder_cost() {
        let bytes = |quanta: u64| quanta * relate_core::tokens::CHARGE_QUANTUM_BYTES;
        let holder = 1_000_000;
        let rows = vec![
            observation(bytes(256), 1, bytes(256), bytes(256) * 2),
            observation(bytes(1024), 1, bytes(1024), bytes(1024) * 2),
            observation(bytes(4096), 1, bytes(4096), bytes(4096) * 2),
            observation(bytes(2048), 2, bytes(4096), bytes(4096) * 2 + holder),
            observation(bytes(1024), 4, bytes(4096), bytes(4096) * 2 + 3 * holder),
        ];
        let summary = summarize_family(Scope::Family(family_spec("create").unwrap()), &rows)
            .expect("summary");
        assert!((summary.rss_single_holder_slope - 2.0).abs() < 1e-9);
        assert!((summary.rss_single_holder_r_squared - 1.0).abs() < 1e-9);
        let per_holder = summary.rss_per_holder_bytes.expect("per-holder cost");
        assert!((per_holder - holder as f64).abs() < 1.0, "{per_holder}");
        assert!(summary.rss_slope > summary.rss_single_holder_slope);
        assert!(summarize_family(Scope::Overall, &rows)
            .expect("summary")
            .rss_per_holder_bytes
            .is_none());
        assert!(summarize_family(Scope::Family(family_spec("sort").unwrap()), &rows).is_err());
    }

    #[test]
    fn observation_serializes_raw_process_and_cgroup_samples() {
        let observation = PeakCalibrationObservation {
            family: "sort".into(),
            target_logical_bytes: 40_960,
            holders: 2,
            repetition: 0,
            held_logical_bytes: 90_112,
            parked_rss_bytes: 100,
            held_rss_bytes: 200,
            rss_delta_bytes: 100,
            rss_bytes_per_logical_byte: 4,
            rss_to_logical_ratio: 0.5,
            parked_cgroup_bytes: Some(300),
            held_cgroup_bytes: Some(500),
            cgroup_delta_bytes: Some(200),
            cgroup_bytes_per_logical_byte: Some(9),
            cgroup_to_logical_ratio: Some(1.0),
        };
        let json = serde_json::to_value(observation).expect("serialize");
        assert_eq!(json["family"], "sort");
        assert_eq!(json["held_logical_bytes"], 90_112);
        assert!(json.get("held_tokens").is_none());
        assert_eq!(json["cgroup_delta_bytes"], 200);
    }
}
