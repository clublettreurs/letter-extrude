//! The side faces of the main extrusion: which ones show, and their two tones.

use serde_json::Value;

use crate::geom::*;
use crate::output::*;
use crate::paint::*;

/// The two cardinal directions bracketing `angle_deg` (`d1`, then `d2 = d1 + 90°`): the two
/// families of faces a cavalier-perspective extrusion at that angle shows on its lateral surface
/// (e.g. 225° brackets to South, then West). `None` at an exact cardinal angle (0/90/180/270°):
/// the whole lateral surface is then a single face, no split needed.
pub(crate) fn face_family(angle_deg: f64) -> Option<(f64, f64)> {
    let q = angle_deg.rem_euclid(360.0) / 90.0;
    if (q - q.round()).abs() < 1e-9 {
        return None;
    }
    let d1 = 90.0 * q.floor();
    Some((d1, (d1 + 90.0).rem_euclid(360.0)))
}

/// How the side faces are shown: which ones are kept and which tone each gets.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Look {
    /// [`face_family`] of `angle`: the two visible cardinal faces, `None` for one uniform tone.
    pub(crate) face: Option<(f64, f64)>,
    /// The extrusion's own compass angle.
    pub(crate) angle: f64,
    /// Which of the two faces is the darker one: the one nearer this bearing (see [`face_tone`]).
    pub(crate) dark_reference: f64,
    /// The outward bearing where the tone changes from dark to light (see [`tone_split`]).
    pub(crate) split: f64,
    /// The cast shadow's own unit direction, when there is one: concave corners cast a shade line.
    pub(crate) shade: Option<P>,
}

impl Look {
    /// One uniform tone, nothing pruned (the cast shadow's own bands).
    pub(crate) fn uniform() -> Self {
        Self { face: None, angle: 0.0, dark_reference: 0.0, split: 0.0, shade: None }
    }

    /// The look of an extrusion at `angle`, with a cast shadow at `shadow_angle` or none.
    pub(crate) fn new(angle: f64, shadow_angle: Option<f64>) -> Self {
        let face = face_family(angle);
        let dark_reference = shadow_angle.unwrap_or(180.0);
        let split = face.map(|(d1, d2)| tone_split(d1, d2, shadow_angle)).unwrap_or(0.0);
        Self { face, angle, dark_reference, split, shade: shadow_angle.map(|s| compass_offset(s, 1.0)) }
    }
}

/// The outward bearing where a curved face changes tone, between the two visible cardinals
/// `d1`/`d2`: where the cast shadow's own outline touches the face (its tangent is parallel to the
/// shadow direction, outward bearing `shadow ± 90°`), so the tone change and the shadow's edge
/// start at the same point; else (no shadow, or that point isn't between the two faces) halfway.
pub(crate) fn tone_split(d1: f64, d2: f64, shadow_angle: Option<f64>) -> f64 {
    let between = |c: f64| circ_dist360(c, d1) < 90.0 && circ_dist360(c, d2) < 90.0;
    shadow_angle
        .into_iter()
        .flat_map(|s| [(s + 90.0).rem_euclid(360.0), (s - 90.0).rem_euclid(360.0)])
        .find(|&c| between(c))
        .unwrap_or((d1 + 45.0).rem_euclid(360.0))
}

/// `a - b` as a signed angle in `(-180, 180]`.
fn signed_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// The true, outward-facing direction a boundary edge running along `tangent` points: a wall
/// faces the direction perpendicular to its own edge's tangent, on whichever side is away from the
/// filled solid material. `dominant_sign` must be the WHOLE OBJECT's own dominant winding sign
/// (its largest-area subpath's, [`signed_area_json`]) — NEVER this one edge's own subpath's sign.
/// A counter/hole is deliberately wound opposite the outer ring it sits inside (that's what makes
/// it subtract under the nonzero fill rule), but "outward" is a property of the WHOLE compound
/// shape's rotation convention, shared by the outer ring and every one of its counters alike: which
/// of the two perpendiculars is "outward" for a given tangent only depends on whether the outer
/// ring itself happens to be wound clockwise or counter-clockwise, not on each individual
/// subpath's own sign (hand-verified both ways: a counter's own edges correctly face into their
/// own hole this way, which a per-subpath sign gets backwards — it would instead face every
/// counter's walls into the solid, hiding them entirely behind the back face and the shadow).
pub(crate) fn outward_bearing(tangent: P, dominant_sign: f64) -> f64 {
    (bearing(tangent) - 90.0 * dominant_sign.signum()).rem_euclid(360.0)
}

/// `None` to drop this boundary piece entirely: its outward direction is more than 90° away from
/// the extrusion's own `angle` (true back-face-culling: an edge is only ever revealed if the
/// offset pushes at least partly "into" the gap it would open up) — keeping only what's actually
/// shown lightens the output as well as the work. Else the tone to paint the kept piece with:
/// `None` for a single uniform tone when `face` is `None` itself (an exact cardinal angle: nothing
/// is pruned then, the whole lateral surface is one face), else `Some(true)` for a piece on the
/// same side of `look.split` as whichever of [`face_family`]'s `d1`/`d2` sits closer to
/// `dark_reference` (darker), `Some(false)` for the other side (lighter). `dark_reference` is the
/// cast shadow's own angle when there is one (the shadow stands in for the scene's light, from the
/// opposite direction: the face nearest the shadow's own direction is the one turned away from
/// that light, hence darker) or due South otherwise. `dominant_sign`: see [`outward_bearing`].
pub(crate) fn face_tone(tangent: P, dominant_sign: f64, look: &Look) -> Option<Option<bool>> {
    let Some((d1, d2)) = look.face else { return Some(None) };
    let normal = outward_bearing(tangent, dominant_sign);
    if circ_dist360(normal, look.angle) > 90.0 {
        return None;
    }
    let dark = if circ_dist360(d1, look.dark_reference) <= circ_dist360(d2, look.dark_reference) { d1 } else { d2 };
    Some(Some(signed_diff(normal, look.split) * signed_diff(dark, look.split) > 0.0))
}

/// The back duplicate, the side bands (each tagged with its own projection along the offset
/// direction for a GLOBAL back-to-front sort across every letter's bands, see [`run`]), and the
/// unchanged front object. At a diagonal `angle`, the bands split into two cavalier-perspective
/// tones (see [`face_family`]): `side_amt` plus or minus half of `contrast`, assigned by
/// `dark_reference` (see [`face_tone`]).
pub(crate) fn extrude_one(
    obj: &Value,
    offset: P,
    side_amt: f64,
    contrast: f64,
    look: &Look,
) -> (Option<OutItem>, Vec<(f64, OutItem)>, Option<OutItem>) {
    let Some(subpaths) = obj["path"]["subpaths"].as_array() else { return (None, vec![], None) };
    let rule: &'static str = if obj["fillRule"].as_str() == Some("evenodd") { "evenodd" } else { "nonzero" };
    let kind: &'static str = if obj["type"].as_str() == Some("compound") { "compound" } else { "path" };
    let paint = top_paint(obj);
    let side_fill = darkened(&paint, side_amt);
    let tone_fills = look
        .face
        .map(|_| (darkened(&paint, (side_amt + contrast / 2.0).clamp(0.0, 1.0)), darkened(&paint, (side_amt - contrast / 2.0).clamp(0.0, 1.0))));
    // Every subpath's own outward direction is found relative to the OBJECT's dominant winding
    // (the actual outer contour's), never its own raw sign: a counter/hole is deliberately wound
    // opposite the outer ring, but it still needs the outer's own rotation convention to find which
    // side of IT faces away from the solid material (see `outward_bearing`'s doc comment).
    let dominant_sign = subpaths.iter().map(signed_area_json).max_by(|a, b| a.abs().total_cmp(&b.abs())).unwrap_or(1.0);

    let len = (offset.0 * offset.0 + offset.1 * offset.1).sqrt().max(1e-9);
    let dir = (offset.0 / len, offset.1 / len);
    let back: Vec<OutSubpath> = subpaths.iter().map(|sp| shift_subpath(sp, offset)).collect();
    let back = OutItem::New(NewObj { kind, path: OutPathData { subpaths: back }, fill_rule: rule, fills: [side_fill.clone()] });

    let fill_for = |tone: Option<bool>| match (tone, &tone_fills) {
        (Some(true), Some((tone1, _))) => tone1.clone(),
        (Some(false), Some((_, tone2))) => tone2.clone(),
        _ => side_fill.clone(),
    };
    let bands = subpaths
        .iter()
        .flat_map(|sp| bands_for_subpath(sp, offset, dominant_sign, look))
        .map(|(sp, tone)| {
            let p = projection(&sp, dir);
            let fill = fill_for(tone);
            (p, OutItem::New(NewObj { kind: "path", path: OutPathData { subpaths: vec![sp] }, fill_rule: "nonzero", fills: [fill] }))
        })
        .collect();

    (Some(back), bands, obj.get("id").map(|id| OutItem::Kept { id: id.clone() }))
}

/// One quadrilateral (or a few curved bands, when the edge is a curve) per boundary edge of `sp`,
/// filling the gap between it and its copy offset by `offset`, each tagged with which of the two
/// cavalier-perspective tones it belongs to (`None` when `face` is `None`: a single uniform tone),
/// or dropped entirely when `face` is `Some` and this piece faces the pair of cardinals the
/// extrusion doesn't reveal (see [`face_tone`]) — EXCEPT a run of consecutive dropped straight
/// edges whose own two flanking KEPT bands would otherwise overlap: such a run is replaced by one
/// band along its own straight-line chord instead (see [`bridge_runs`]). Dropping every edge of a
/// short, sharply-turning (reflex) notch independently can leave its two kept neighbours no
/// longer literally adjacent (they'd no longer share a drawn edge), which is what lets their own
/// swept bands cross each other there instead of meeting cleanly; actually checking whether that
/// crossing happens (not just guessing from the chord's own direction, which is iDENTICAL to the
/// dropped edge's own for a single-edge gap, and so never distinguishes the two cases) is what
/// tells a genuine reflex notch like that apart from an ordinary long run of hidden edges (e.g. a
/// square's whole far side), which must stay dropped, not bridged. `dominant_sign` is the whole
/// object's own dominant winding sign ([`signed_area_json`] of its largest-area subpath), not
/// `sp`'s own: see [`outward_bearing`].
pub(crate) fn bands_for_subpath(sp: &Value, offset: P, dominant_sign: f64, look: &Look) -> Vec<(OutSubpath, Option<bool>)> {
    let anchors = sp.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default();
    let closed = sp.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let n = anchors.len();
    if n < 2 {
        return vec![];
    }
    let segments = if closed { n } else { n - 1 };
    let edge_points = |i: usize| -> Option<(P, P)> {
        let (a, b) = (anchors.get(i % n)?, anchors.get((i + 1) % n)?);
        Some((point(a, "p")?, point(b, "p")?))
    };
    let is_dropped_straight = |i: usize| -> bool {
        let (Some(a), Some(b)) = (anchors.get(i % n), anchors.get((i + 1) % n)) else { return false };
        if point(a, "out").is_some() || point(b, "in").is_some() {
            // A curve's own pieces are classified individually below, not merged into a run: two
            // consecutive pieces of the SAME curve stay literally adjacent either way (they share
            // an exact split point), so they already tile without this fix being needed for them.
            return false;
        }
        let Some((pa, pb)) = edge_points(i) else { return false };
        face_tone((pb.0 - pa.0, pb.1 - pa.1), dominant_sign, look).is_none()
    };
    // The direction the boundary arrives at / leaves anchor `i`, for telling concave corners.
    let incoming = |i: usize| -> Option<P> {
        let a = anchors.get(i % n)?;
        let p = point(a, "p")?;
        let from = match point(a, "in") {
            Some(h) => h,
            None if closed || i > 0 => point(anchors.get((i + n - 1) % n)?, "p")?,
            None => return None,
        };
        Some((p.0 - from.0, p.1 - from.1))
    };
    let outgoing = |i: usize| -> Option<P> {
        let a = anchors.get(i % n)?;
        let p = point(a, "p")?;
        let to = match point(a, "out") {
            Some(h) => h,
            None if closed || i + 1 < n => point(anchors.get((i + 1) % n)?, "p")?,
            None => return None,
        };
        Some((to.0 - p.0, to.1 - p.1))
    };
    let concave = |i: usize| match (incoming(i), outgoing(i)) {
        (Some(e_in), Some(e_out)) => cross(e_in, e_out) * dominant_sign < 0.0,
        _ => false,
    };
    bridge_runs(segments, closed, is_dropped_straight)
        .into_iter()
        .flat_map(|piece| match piece {
            BandPiece::Bridge(i0, i1) => {
                let (Some(a), Some(b)) = (anchors.get(i0 % n).and_then(|a| point(a, "p")), anchors.get(i1 % n).and_then(|a| point(a, "p"))) else {
                    return vec![];
                };
                let Some((d1, d2)) = look.face else { return vec![] };
                if a == b {
                    return vec![];
                }
                // Only actually bridge when skipping the run would otherwise make its two kept
                // neighbours cross: a plain quad for each (ignoring any curve handles, just an
                // approximation to decide whether to bridge at all).
                let quad = |(a, b): (P, P)| [a, b, add(b, offset), add(a, offset)];
                let (Some(before), Some(after)) = (edge_points((i0 + segments - 1) % segments).map(quad), edge_points(i1).map(quad)) else {
                    return vec![];
                };
                if !quads_overlap(&before, &after) {
                    return vec![];
                }
                let normal = outward_bearing((b.0 - a.0, b.1 - a.1), dominant_sign);
                let tone = Some(circ_dist360(normal, d1) <= circ_dist360(normal, d2));
                vec![(band(a, None, b, None, offset), tone)]
            }
            BandPiece::Edge(i) => {
                let Some(a) = anchors.get(i % n) else { return vec![] };
                let Some(b) = anchors.get((i + 1) % n) else { return vec![] };
                let (Some(pa), Some(pb)) = (point(a, "p"), point(b, "p")) else { return vec![] };
                let bands = segment_bands(pa, point(a, "out"), pb, point(b, "in"), offset, dominant_sign, look);
                let straight = point(a, "out").is_none() && point(b, "in").is_none();
                match (look.shade, bands.as_slice()) {
                    (Some(s), [(_, Some(false))]) if straight => {
                        let shaded = (concave(i).then(|| shade_split(pa, pb, true, offset, s)))
                            .flatten()
                            .or_else(|| concave((i + 1) % n).then(|| shade_split(pa, pb, false, offset, s)).flatten());
                        shaded.unwrap_or(bands)
                    }
                    _ => bands,
                }
            }
        })
        .collect()
}

/// The lit band of straight edge `a` → `b`, split by the shade line a concave corner casts on it:
/// from the corner (`a` when `at_a`, else `b`), along the shadow direction `shade`, to where it
/// leaves the band (on its back edge, or on the far connector). The part between that line and
/// the corner's own connector is in the shade of the other edge meeting there (e.g. a "T"'s bar on
/// its stem), so it takes the dark tone. `None` when the shade line doesn't cross the band.
pub(crate) fn shade_split(a: P, b: P, at_a: bool, offset: P, shade: P) -> Option<Vec<(OutSubpath, Option<bool>)>> {
    let (v, w) = if at_a { (a, b) } else { (b, a) };
    let e = (w.0 - v.0, w.1 - v.1);
    let det = cross(e, offset);
    if det.abs() < 1e-12 {
        return None;
    }
    // `shade` = along·e + depth·offset, in the band's own coordinates.
    let along = cross(shade, offset) / det;
    let depth = cross(e, shade) / det;
    if along <= 1e-9 || depth <= 1e-9 {
        return None;
    }
    let (v2, w2) = (add(v, offset), add(w, offset));
    let hit = |t: f64| (v.0 + shade.0 * t, v.1 + shade.1 * t);
    let (dark, light) = if along / depth < 1.0 {
        // Leaves through the back edge.
        let p = hit(1.0 / depth);
        (vec![v, p, v2], vec![v, w, w2, p])
    } else {
        // Leaves through the far connector.
        let q = hit(1.0 / along);
        (vec![v, q, w2, v2], vec![v, w, q])
    };
    let poly = |pts: Vec<P>| OutSubpath { anchors: pts.into_iter().map(|p| out_anchor(p, None, None)).collect(), closed: true };
    Some(vec![(poly(light), Some(false)), (poly(dark), Some(true))])
}

/// A normal boundary edge, processed as usual, or a bridge replacing a run of dropped ones: see
/// [`bridge_runs`].
pub(crate) enum BandPiece {
    Edge(usize),
    Bridge(usize, usize),
}

/// Splits `segments` consecutive boundary edges into normal per-edge pieces and "bridges": a
/// bridge replaces a MAXIMAL RUN of 1+ consecutive edges `is_dropped` reports as fully hidden,
/// from the run's own first vertex straight to its last, forced to stay visible (never itself
/// dropped) — see [`bands_for_subpath`]'s doc comment for why. Vertex indices in a returned
/// `Bridge` may be `>= segments` (wrapping around a closed subpath): take them `% segments` (or
/// `% anchors.len()`) before indexing.
pub(crate) fn bridge_runs(segments: usize, closed: bool, is_dropped: impl Fn(usize) -> bool) -> Vec<BandPiece> {
    if segments == 0 {
        return vec![];
    }
    let dropped: Vec<bool> = (0..segments).map(&is_dropped).collect();
    if closed && dropped.iter().all(|&d| d) {
        return vec![];
    }
    // Start the scan at a kept edge for a closed subpath, so a run is never split by having to
    // wrap past the end of the list back to the start.
    let start = if closed { dropped.iter().position(|&d| !d).unwrap_or(0) } else { 0 };
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < segments {
        let idx = (start + i) % segments;
        if !dropped[idx] {
            pieces.push(BandPiece::Edge(idx));
            i += 1;
            continue;
        }
        let run_start = idx;
        let mut len = 1;
        while len < segments && (closed || start + i + len < segments) && dropped[(start + i + len) % segments] {
            len += 1;
        }
        pieces.push(BandPiece::Bridge(run_start, run_start + len));
        i += len;
    }
    pieces
}

/// The band(s) between edge `a`(`a_out`) → `b`(`b_in`) and its copy offset by `offset`: pruned
/// entirely (an empty `Vec`) when every bit of it faces away from `angle` (see [`face_tone`]),
/// else tagged with which cavalier-perspective tone each kept piece belongs to (`true` for the
/// tone nearer `dark_reference`, `false` for the other; `None` when `face` is `None`, a single
/// uniform tone, nothing pruned). `sign` is the parent subpath's own winding sign, needed by
/// [`face_tone`] to find each piece's true outward direction. One straight-sided band for a
/// straight edge. For a curved one, one band per piece between [`tangent_crossings`] of the offset
/// direction — self-intersection avoidance AND the exact visibility boundary at once, since a
/// piece's outward direction is exactly 90° from `angle` precisely where its tangent is parallel
/// to `offset` — and, when two-tone, of the tone boundary (`d1+45°`, the midpoint between the two
/// visible faces) — the curve's true shape on both sides, not a polygon approximation of it, split
/// only where the sweep's own direction, tone or visibility actually changes.
pub(crate) fn segment_bands(a: P, a_out: Option<P>, b: P, b_in: Option<P>, offset: P, sign: f64, look: &Look) -> Vec<(OutSubpath, Option<bool>)> {
    if a_out.is_none() && b_in.is_none() {
        let Some(tone) = face_tone((b.0 - a.0, b.1 - a.1), sign, look) else { return vec![] };
        return vec![(band(a, None, b, None, offset), tone)];
    }
    let (p1, p2) = (a_out.unwrap_or(a), b_in.unwrap_or(b));
    let mut ts = vec![0.0];
    ts.extend(tangent_crossings(a, p1, p2, b, offset));
    if look.face.is_some() {
        let unit = {
            let len = (offset.0 * offset.0 + offset.1 * offset.1).sqrt().max(1e-9);
            (offset.0 / len, offset.1 / len)
        };
        // The tone changes where the outward bearing crosses `look.split`, i.e. where the tangent
        // is perpendicular to it. Skip when that's the offset's own line: already covered above.
        let dir = compass_offset(look.split + 90.0, 1.0);
        if cross(dir, unit).abs() > 1e-9 {
            ts.extend(tangent_crossings(a, p1, p2, b, dir));
        }
    }
    ts.push(1.0);
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|x, y| (*x - *y).abs() < 1e-7);
    ts.windows(2)
        .filter_map(|w| {
            let [q0, q1, q2, q3] = sub_cubic(a, p1, p2, b, w[0], w[1]);
            let tone = face_tone(cubic_tangent(a, p1, p2, b, (w[0] + w[1]) / 2.0), sign, look)?;
            Some((band(q0, Some(q1), q3, Some(q2), offset), tone))
        })
        .collect()
}
