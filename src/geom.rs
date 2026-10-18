//! Plain 2D geometry: points, compass bearings, cubic bezier splitting and tangents.

use serde_json::Value;

/// A point in the document's own coordinates (points, y down).
pub(crate) type P = (f64, f64);

pub(crate) fn add(p: P, d: P) -> P {
    (p.0 + d.0, p.1 + d.1)
}

/// A compass angle (0° and 360° both North, 90° East, 180° South, 270° West, clockwise), wrapped
/// into a `distance`-long offset in the document's own coordinates (points, y down). Wraps at
/// either end of the circle (370° or -10° are both 10°/North-north-east), so the angle is always a
/// position on the full 360° dial, however it is given.
pub(crate) fn compass_offset(angle_deg: f64, distance: f64) -> P {
    let a = angle_deg.rem_euclid(360.0).to_radians();
    // sin/cos of the cardinal points (0/90/180/270°) aren't exactly 0 in floating point (π isn't
    // exact either): snapped, so East/West/North/South give a perfectly axis-aligned offset.
    let snap = |v: f64| if v.abs() < 1e-9 { 0.0 } else { v };
    (snap(distance * a.sin()), snap(-distance * a.cos()))
}

/// The 2D cross product (scalar): positive when `b` is counter-clockwise from `a`.
pub(crate) fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

/// `v`'s own compass bearing (0°=North/up, 90°=East/right, clockwise), in `[0, 360)` — the
/// inverse of [`compass_offset`]'s direction.
pub(crate) fn bearing(v: P) -> f64 {
    v.0.atan2(-v.1).to_degrees().rem_euclid(360.0)
}

/// The shortest distance between two bearings (`0..=180°`).
pub(crate) fn circ_dist360(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

pub(crate) fn lerp(a: P, b: P, t: f64) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// De Casteljau split of the cubic bezier `p0 p1 p2 p3` at `t`: the control points of `[0, t]` and
/// `[t, 1]`.
pub(crate) fn split_cubic(p0: P, p1: P, p2: P, p3: P, t: f64) -> ([P; 4], [P; 4]) {
    let e0 = lerp(p0, p1, t);
    let e1 = lerp(p1, p2, t);
    let e2 = lerp(p2, p3, t);
    let f0 = lerp(e0, e1, t);
    let f1 = lerp(e1, e2, t);
    let g = lerp(f0, f1, t);
    ([p0, e0, f0, g], [g, f1, e2, p3])
}

/// The control points of the cubic bezier `p0 p1 p2 p3` restricted to `[t0, t1]` (`0 <= t0 < t1 <=
/// 1`): the same curve exactly, broken into a smaller piece.
pub(crate) fn sub_cubic(p0: P, p1: P, p2: P, p3: P, t0: f64, t1: f64) -> [P; 4] {
    let (_, from_t0) = split_cubic(p0, p1, p2, p3, t0);
    let span = (1.0 - t0).max(1e-9);
    let (upto_t1, _) = split_cubic(from_t0[0], from_t0[1], from_t0[2], from_t0[3], ((t1 - t0) / span).clamp(0.0, 1.0));
    upto_t1
}

/// The cubic bezier `p0 p1 p2 p3`'s own tangent direction at `t` (unnormalized): `B'(t) =
/// 3(1-t)²(p1-p0) + 6(1-t)t(p2-p1) + 3t²(p3-p2)`.
pub(crate) fn cubic_tangent(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let v0 = (p1.0 - p0.0, p1.1 - p0.1);
    let v1 = (p2.0 - p1.0, p2.1 - p1.1);
    let v2 = (p3.0 - p2.0, p3.1 - p2.1);
    let mt = 1.0 - t;
    let (k0, k1, k2) = (3.0 * mt * mt, 6.0 * mt * t, 3.0 * t * t);
    (k0 * v0.0 + k1 * v1.0 + k2 * v2.0, k0 * v0.1 + k1 * v1.1 + k2 * v2.1)
}

/// The parameter values strictly inside `(0, 1)` where the cubic bezier `p0 p1 p2 p3`'s own
/// tangent is exactly parallel to `offset`: the points where the swept band between the curve and
/// its copy offset by `offset` must hand off from following the curve itself to following its
/// offset copy (or back) to stay a simple, non-self-intersecting shape — found in closed form
/// instead of approximated by sampling the curve at uniform steps. A cubic's derivative is a
/// quadratic in `t` (`C'(t)/3 = qc + t·qb + t²·qa`), so `C'(t) × offset = 0` is a plain quadratic
/// equation: at most 2 real roots, so a curved edge never needs more than 3 bands.
pub(crate) fn tangent_crossings(p0: P, p1: P, p2: P, p3: P, offset: P) -> Vec<f64> {
    let v0 = (p1.0 - p0.0, p1.1 - p0.1);
    let v1 = (p2.0 - p1.0, p2.1 - p1.1);
    let v2 = (p3.0 - p2.0, p3.1 - p2.1);
    let qa = (v0.0 - 2.0 * v1.0 + v2.0, v0.1 - 2.0 * v1.1 + v2.1);
    let qb = (2.0 * (v1.0 - v0.0), 2.0 * (v1.1 - v0.1));
    let qc = v0;
    let (a, b, c) = (cross(qa, offset), cross(qb, offset), cross(qc, offset));
    const EPS: f64 = 1e-9;
    let in_range = |t: f64| t > EPS && t < 1.0 - EPS;
    if a.abs() < EPS {
        return if b.abs() < EPS { vec![] } else { Some(-c / b).filter(|&t| in_range(t)).into_iter().collect() };
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return vec![];
    }
    let sq = disc.sqrt();
    let mut ts: Vec<f64> = [(-b + sq) / (2.0 * a), (-b - sq) / (2.0 * a)].into_iter().filter(|&t| in_range(t)).collect();
    ts.sort_by(f64::total_cmp);
    ts
}

/// Whether convex quads `q1`/`q2` (each 4 points, in order) overlap in AREA, by the separating
/// axis theorem: they don't if some axis perpendicular to one of the 8 edges has their two
/// projections only touching or apart, with a small tolerance so two quads that are merely
/// adjacent (sharing a full edge, as two directly kept neighbours legitimately do: see
/// [`bands_for_subpath`]'s doc comment) read as NOT overlapping, even right at a shared VERTEX
/// (where a plain point-in-polygon test on the raw corners is ambiguous) — good enough to decide
/// whether a bridge is needed, not a general-purpose exact intersection test.
pub(crate) fn quads_overlap(q1: &[P; 4], q2: &[P; 4]) -> bool {
    const EPS: f64 = 1e-6;
    for q in [q1, q2] {
        for i in 0..4 {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            let axis = (-(b.1 - a.1), b.0 - a.0);
            let len = (axis.0 * axis.0 + axis.1 * axis.1).sqrt();
            if len < 1e-12 {
                continue;
            }
            let axis = (axis.0 / len, axis.1 / len);
            let project = |q: &[P; 4]| -> (f64, f64) {
                q.iter().map(|p| p.0 * axis.0 + p.1 * axis.1).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
            };
            let ((lo1, hi1), (lo2, hi2)) = (project(q1), project(q2));
            if hi1 <= lo2 + EPS || hi2 <= lo1 + EPS {
                return false;
            }
        }
    }
    true
}

/// `v[key]` as `(x, y)`; absent for `"in"`/`"out"` means no handle.
pub(crate) fn point(v: &Value, key: &str) -> Option<P> {
    let a = v.get(key)?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// The average of `anchors`' own points (ignoring curvature): a representative point known to sit
/// well inside a typical (roughly convex, as real letterforms' counters are) closed subpath, for
/// [`point_in_polygon`] to test against another one.
pub(crate) fn centroid(anchors: &[Value]) -> P {
    let (sx, sy, n) = anchors.iter().filter_map(|a| point(a, "p")).fold((0.0, 0.0, 0.0_f64), |(sx, sy, n), p| (sx + p.0, sy + p.1, n + 1.0));
    if n == 0.0 { (0.0, 0.0) } else { (sx / n, sy / n) }
}

/// Whether `p` lies inside the closed polygon formed by `anchors`' own points (ignoring curvature,
/// the same approximation [`signed_area_json`] already makes): a standard ray-casting test, used
/// to tell a counter from a separate letterform piece regardless of which way either one winds.
pub(crate) fn point_in_polygon(p: P, anchors: &[Value]) -> bool {
    let n = anchors.len();
    let mut inside = false;
    for i in 0..n {
        let (Some(a), Some(b)) = (point(&anchors[i], "p"), point(&anchors[(i + 1) % n], "p")) else { continue };
        if (a.1 > p.1) != (b.1 > p.1) {
            let x_at_p_y = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if p.0 < x_at_p_y {
                inside = !inside;
            }
        }
    }
    inside
}

/// The shoelace signed area of a subpath's anchor `"p"` points, read straight from input JSON.
pub(crate) fn signed_area_json(sp: &Value) -> f64 {
    let Some(anchors) = sp.get("anchors").and_then(Value::as_array) else { return 0.0 };
    let n = anchors.len();
    (0..n)
        .map(|i| {
            let (Some(a), Some(b)) = (point(&anchors[i], "p"), point(&anchors[(i + 1) % n], "p")) else { return 0.0 };
            a.0 * b.1 - b.0 * a.1
        })
        .sum()
}
