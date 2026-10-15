//! Lettering Extrude: a VectorCraft object filter plug-in (plug-in ABI v1, see `docs/plugins.md`).
//!
//! Reproduces the classic hand-lettering "fake perspective" extrusion: duplicate the letterform,
//! offset the copy backwards by an angle and a distance, and fill the gap between the two copies
//! with one quadrilateral (or curved band, following the original curve) per boundary edge. Unlike
//! the built-in Extrude & Bevel effect (a true 3D rotation, which also tilts the front face to
//! reveal depth), the front face here stays exactly where the art is: only a 2D offset simulates
//! depth, as in the manual duplicate-and-connect technique. Every back face and side band is a
//! plain, separate output object, so each one is independently selectable, recolourable or
//! deletable after the filter runs — nothing is a "live" effect.
//!
//! The anchors of every back face and band are built as plain structs and serialized straight to
//! JSON bytes (never through a `serde_json::Value` tree): building each one as a `Value` (a
//! `String`-keyed map, allocated per field) costs enough interpreted instructions in the plug-in
//! sandbox to exceed its budget at any real scale. Only `serde` and `serde_json` are used: no
//! imports, no WASI.

use serde::Serialize;
use serde_json::{json, Value};

const MANIFEST: &str = r#"{
  "id": "lettering.extrude2d",
  "name": "Lettering Extrude",
  "version": "1.0.0",
  "kind": "filter",
  "author": "VectorCraft contributors",
  "description": "Duplicates the selected letterforms, offsets the copy by an angle and distance, and builds the connecting side faces as separate, selectable objects: the classic hand-lettering fake-perspective extrusion.",
  "params": {
    "angle": {"type": "number", "min": 0, "max": 360, "default": 225},
    "distance": {"type": "number", "min": 1, "max": 2000, "default": 40},
    "sideShade": {"type": "number", "min": 0, "max": 100, "default": 35},
    "shadow": {"type": "bool", "default": false},
    "shadowAngle": {"type": "number", "min": 0, "max": 360, "default": 115},
    "shadowDistance": {"type": "number", "min": 1, "max": 2000, "default": 50}
  }
}"#;

#[unsafe(no_mangle)]
pub extern "C" fn vc_abi_version() -> u32 {
    1
}

/// `len << 32 | ptr` of the UTF-8 manifest JSON.
#[unsafe(no_mangle)]
pub extern "C" fn vc_manifest() -> u64 {
    pack(MANIFEST.as_bytes())
}

/// A block of `size` bytes the host writes the input and the parameters into. Never freed: every
/// run gets a fresh instance.
#[unsafe(no_mangle)]
pub extern "C" fn vc_alloc(size: u32) -> u32 {
    let mut block = Vec::<u8>::with_capacity(size as usize);
    let ptr = block.as_mut_ptr();
    std::mem::forget(block);
    ptr as u32
}

/// Runs the filter: `input` holds `{"objects": [...]}`, `params` the parameters. Returns the
/// output JSON as `len << 32 | ptr`, or a negative error code.
///
/// # Safety
/// The host guarantees both blocks came from `vc_alloc` and hold `*_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_run(input: *const u8, input_len: u32, params: *const u8, params_len: u32) -> i64 {
    // SAFETY: see the function's contract.
    let (input, params) = unsafe { (std::slice::from_raw_parts(input, input_len as usize), std::slice::from_raw_parts(params, params_len as usize)) };
    let (Ok(doc), Ok(params)) = (serde_json::from_slice::<Value>(input), serde_json::from_slice::<Value>(params)) else { return -1 };
    let bytes = run(&doc, &params).leak();
    pack(bytes) as i64
}

/// A point in the document's own coordinates (points, y down).
type P = (f64, f64);

fn add(p: P, d: P) -> P {
    (p.0 + d.0, p.1 + d.1)
}

/// A compass angle (0° and 360° both North, 90° East, 180° South, 270° West, clockwise), wrapped
/// into a `distance`-long offset in the document's own coordinates (points, y down). Wraps at
/// either end of the circle (370° or -10° are both 10°/North-north-east), so the angle is always a
/// position on the full 360° dial, however it is given.
fn compass_offset(angle_deg: f64, distance: f64) -> P {
    let a = angle_deg.rem_euclid(360.0).to_radians();
    // sin/cos of the cardinal points (0/90/180/270°) aren't exactly 0 in floating point (π isn't
    // exact either): snapped, so East/West/North/South give a perfectly axis-aligned offset.
    let snap = |v: f64| if v.abs() < 1e-9 { 0.0 } else { v };
    (snap(distance * a.sin()), snap(-distance * a.cos()))
}

/// One anchor of a generated back face or band: a plain struct, not a `Value`, so serializing
/// thousands of them (one letterform's worth of bands) writes their field names straight from
/// `&'static str` instead of allocating a `String` key per field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
struct OutAnchor {
    p: [f64; 2],
    #[serde(rename = "in", skip_serializing_if = "Option::is_none")]
    h_in: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<[f64; 2]>,
}

fn out_anchor(p: P, h_in: Option<P>, h_out: Option<P>) -> OutAnchor {
    OutAnchor { p: [p.0, p.1], h_in: h_in.map(|h| [h.0, h.1]), out: h_out.map(|h| [h.0, h.1]) }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct OutSubpath {
    anchors: Vec<OutAnchor>,
    closed: bool,
}

#[derive(Serialize)]
struct OutPathData {
    subpaths: Vec<OutSubpath>,
}

/// A new path object: the back face or one side band. `fills` still carries the paint as a
/// `Value` (arbitrary solid/gradient/pattern JSON), but there is only one per object, not one per
/// anchor, so its cost doesn't scale with the number of bands.
#[derive(Serialize)]
struct NewObj {
    #[serde(rename = "type")]
    kind: &'static str,
    path: OutPathData,
    #[serde(rename = "fillRule")]
    fill_rule: &'static str,
    fills: [Value; 1],
}

#[derive(Serialize)]
#[serde(untagged)]
enum OutItem {
    New(NewObj),
    Kept { id: Value },
}

#[derive(Serialize)]
struct Output {
    objects: Vec<OutItem>,
}

const NO_SELECTION: &[u8] = br#"{"error":"Select one or more letterforms (paths or compound paths) to extrude."}"#;

/// The filter's own logic, independent of the WASM boundary so it can be unit-tested natively.
/// Returns the output JSON bytes directly (never an intermediate `Value` tree): see the module
/// doc comment.
fn run(doc: &Value, params: &Value) -> Vec<u8> {
    let objects = doc["objects"].as_array().cloned().unwrap_or_default();
    if objects.is_empty() {
        return NO_SELECTION.to_vec();
    }
    let angle = params["angle"].as_f64().unwrap_or(225.0);
    let distance = params["distance"].as_f64().unwrap_or(40.0).clamp(1.0, 2000.0);
    let side_amt = (params["sideShade"].as_f64().unwrap_or(35.0) / 100.0).clamp(0.0, 1.0);
    let offset = compass_offset(angle, distance);
    let shadow_on = params["shadow"].as_bool().unwrap_or(false);
    let valid: Vec<&Value> = objects.iter().filter(|o| o.get("path").is_some()).collect();

    // The cast shadow (one merged shape, every letter's outer silhouette — never its counters —
    // extruded again towards the shadow's own angle and distance) goes first: under the depth and
    // the front faces, at the very back of the whole selection.
    let mut result = vec![];
    if shadow_on {
        let shadow_angle = params["shadowAngle"].as_f64().unwrap_or(115.0);
        let shadow_distance = params["shadowDistance"].as_f64().unwrap_or(50.0).clamp(1.0, 2000.0);
        result.extend(shadow_object(&valid, offset, compass_offset(shadow_angle, shadow_distance), side_amt));
    }

    // Every letter's depth (back face + side bands) is stacked below every letter's front face, in
    // that order, whichever object comes first: with several letters (e.g. a word typed left to
    // right), one letter's depth must never cover a neighbour's front face.
    let (mut depth, mut fronts) = (vec![], vec![]);
    for obj in &valid {
        let (back_and_bands, front) = extrude_one(obj, offset, side_amt);
        depth.extend(back_and_bands);
        fronts.extend(front);
    }
    if result.is_empty() && depth.is_empty() && fronts.is_empty() {
        return NO_SELECTION.to_vec();
    }
    result.extend(depth);
    result.extend(fronts);
    serde_json::to_vec(&Output { objects: result }).unwrap_or_else(|_| NO_SELECTION.to_vec())
}

/// The cast shadow: every letter's own back face (outer contour AND counters alike — "contre-
/// formes", the hole of an "O", "A", "B" or "P" — nothing is excluded) kept exactly where the main
/// back face already put it, extruded again towards `shadow_offset` by the same principle as the
/// main extrusion — a second back face and the bands connecting the two, every one of them wound
/// the same way as its own back face so a shared fillRule never cancels a piece out against
/// ITSELF (this is as far as "Pathfinder Unite" goes here: the plug-in sandbox has no access to
/// the host's own boolean engine, and working per edge like this never depends on the shape being
/// convex, closed, or free of the sharp interior angles some fonts' outlines have). Every piece,
/// from every letter, is merged into ONE compound object: normalizing every letterform piece to
/// one reference winding, and every COUNTER piece to the OPPOSITE one, makes several overlapping
/// letters' outer silhouettes read as one seamless union (no seam at their shared boundary) while
/// each counter keeps subtracting — staying transparent — exactly like the original glyph's own
/// fillRule already does. Filled with a flat grey as dark as `side_amt` (the side bands' own
/// shading). `None` when nothing in `objects` has geometry to cast one.
///
/// Known limitation (inherent to merging everything under one shared fillRule with no access to a
/// real boolean engine): if one letter's counter happens to spatially overlap a DIFFERENT letter's
/// own solid material (very tightly overlapping letters), that overlap can read as transparent
/// instead of solid. Ordinary side-by-side or touching lettering, where a counter sits well inside
/// its own letter, is unaffected.
fn shadow_object(objects: &[&Value], main_offset: P, shadow_offset: P, side_amt: f64) -> Option<OutItem> {
    let mut subpaths: Vec<(OutSubpath, bool)> = vec![];
    for (sp, is_outer) in objects.iter().flat_map(|o| subpaths_with_role(o)) {
        let back = shift_subpath(&sp, main_offset);
        let sign = signed_area(&back);
        subpaths.extend(bands_for_out_subpath(&back, shadow_offset).into_iter().map(|b| (wound_like(b, sign), is_outer)));
        subpaths.push((shift_out_subpath(&back, shadow_offset), is_outer));
        subpaths.push((back, is_outer));
    }
    if subpaths.is_empty() {
        return None;
    }
    // Every letterform piece (across every letter) shares one winding, so they simply union
    // together; every counter piece shares the OPPOSITE one, so it keeps subtracting from whatever
    // letterform material it overlaps (its own letter's, almost always).
    let reference = subpaths.iter().find(|(_, is_outer)| *is_outer).map(|(sp, _)| signed_area(sp)).unwrap_or(1.0);
    let subpaths: Vec<OutSubpath> =
        subpaths.into_iter().map(|(sp, is_outer)| wound_like(sp, if is_outer { reference } else { -reference })).collect();
    // The same grey intensity as the side bands (sideShade), not the letters' own colour.
    let fill = json!({"type": "solid", "color": {"model": "gray", "k": side_amt}});
    Some(OutItem::New(NewObj { kind: "compound", path: OutPathData { subpaths }, fill_rule: "nonzero", fills: [fill] }))
}

/// `obj`'s subpaths, each tagged with whether it's letterform (`true`, same winding sign as the
/// largest one by area — e.g. both the dot and the stem of "i", which share no counter) or one of
/// its counters (`false`, wound the other way — e.g. the hole of "O", "A", "B" or "P").
fn subpaths_with_role(obj: &Value) -> Vec<(Value, bool)> {
    let Some(subpaths) = obj["path"]["subpaths"].as_array() else { return vec![] };
    let areas: Vec<f64> = subpaths.iter().map(signed_area_json).collect();
    let Some(dominant) = areas.iter().max_by(|a, b| a.abs().total_cmp(&b.abs())).map(|a| a.signum()) else { return vec![] };
    subpaths.iter().zip(&areas).map(|(sp, a)| (sp.clone(), a.signum() == dominant)).collect()
}

/// The shoelace signed area of a subpath's anchor `"p"` points, read straight from input JSON.
fn signed_area_json(sp: &Value) -> f64 {
    let Some(anchors) = sp.get("anchors").and_then(Value::as_array) else { return 0.0 };
    let n = anchors.len();
    (0..n)
        .map(|i| {
            let (Some(a), Some(b)) = (point(&anchors[i], "p"), point(&anchors[(i + 1) % n], "p")) else { return 0.0 };
            a.0 * b.1 - b.0 * a.1
        })
        .sum()
}

/// The shoelace signed area of `sp`'s anchor points (ignoring curvature, which doesn't change the
/// sign): which way it winds.
fn signed_area(sp: &OutSubpath) -> f64 {
    let n = sp.anchors.len();
    (0..n)
        .map(|i| {
            let (a, b) = (sp.anchors[i].p, sp.anchors[(i + 1) % n].p);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// `sp`, reversed (same shape, opposite winding) when its own signed area doesn't have the same
/// sign as `reference`.
fn wound_like(sp: OutSubpath, reference: f64) -> OutSubpath {
    if signed_area(&sp) * reference < 0.0 {
        reverse_out_subpath(sp)
    } else {
        sp
    }
}

/// The same closed shape traced the other way: anchors in reverse order, each with its own
/// in/out handles swapped (as the native path model's own `reverse` does).
fn reverse_out_subpath(mut sp: OutSubpath) -> OutSubpath {
    sp.anchors.reverse();
    for a in &mut sp.anchors {
        std::mem::swap(&mut a.h_in, &mut a.out);
    }
    sp
}

/// The back duplicate and the side bands (sorted back to front), and the unchanged front object,
/// for one input path or compound path.
fn extrude_one(obj: &Value, offset: P, side_amt: f64) -> (Vec<OutItem>, Option<OutItem>) {
    let Some(subpaths) = obj["path"]["subpaths"].as_array() else { return (vec![], None) };
    let rule: &'static str = if obj["fillRule"].as_str() == Some("evenodd") { "evenodd" } else { "nonzero" };
    let kind: &'static str = if obj["type"].as_str() == Some("compound") { "compound" } else { "path" };
    let paint = top_paint(obj);
    let side_fill = darkened(&paint, side_amt);

    let back = subpaths.iter().map(|sp| shift_subpath(sp, offset)).collect();
    let mut result = vec![OutItem::New(NewObj { kind, path: OutPathData { subpaths: back }, fill_rule: rule, fills: [side_fill.clone()] })];

    let len = (offset.0 * offset.0 + offset.1 * offset.1).sqrt().max(1e-9);
    let dir = (offset.0 / len, offset.1 / len);
    let mut bands: Vec<(f64, OutSubpath)> =
        subpaths.iter().flat_map(|sp| bands_for_subpath(sp, offset)).map(|sp| (projection(&sp, dir), sp)).collect();
    // Furthest along the offset direction (most hidden) first, so later bands and the front face
    // layer correctly on top.
    bands.sort_by(|a, b| b.0.total_cmp(&a.0));
    result.extend(bands.into_iter().map(|(_, sp)| {
        OutItem::New(NewObj { kind: "path", path: OutPathData { subpaths: vec![sp] }, fill_rule: "nonzero", fills: [side_fill.clone()] })
    }));

    (result, obj.get("id").map(|id| OutItem::Kept { id: id.clone() }))
}

/// One quadrilateral (or a few curved bands, when the edge is a curve) per boundary edge of `sp`,
/// filling the gap between it and its copy offset by `offset`.
fn bands_for_subpath(sp: &Value, offset: P) -> Vec<OutSubpath> {
    let anchors = sp.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default();
    let closed = sp.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let n = anchors.len();
    if n < 2 {
        return vec![];
    }
    let segments = if closed { n } else { n - 1 };
    (0..segments)
        .filter_map(|i| {
            let a = anchors.get(i % n)?;
            let b = anchors.get((i + 1) % n)?;
            Some(segment_bands(point(a, "p")?, point(a, "out"), point(b, "p")?, point(b, "in"), offset))
        })
        .flatten()
        .collect()
}

/// The same as [`bands_for_subpath`], but reading a subpath already built as an [`OutSubpath`]
/// (e.g. a back face) instead of input JSON: used to extrude the back face again for the shadow.
fn bands_for_out_subpath(sp: &OutSubpath, offset: P) -> Vec<OutSubpath> {
    let n = sp.anchors.len();
    if n < 2 {
        return vec![];
    }
    let segments = if sp.closed { n } else { n - 1 };
    (0..segments)
        .filter_map(|i| {
            let a = sp.anchors.get(i % n)?;
            let b = sp.anchors.get((i + 1) % n)?;
            Some(segment_bands((a.p[0], a.p[1]), a.out.map(|h| (h[0], h[1])), (b.p[0], b.p[1]), b.h_in.map(|h| (h[0], h[1])), offset))
        })
        .flatten()
        .collect()
}

/// The 2D cross product (scalar): positive when `b` is counter-clockwise from `a`.
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

/// The parameter values strictly inside `(0, 1)` where the cubic bezier `p0 p1 p2 p3`'s own
/// tangent is exactly parallel to `offset`: the points where the swept band between the curve and
/// its copy offset by `offset` must hand off from following the curve itself to following its
/// offset copy (or back) to stay a simple, non-self-intersecting shape — found in closed form
/// instead of approximated by sampling the curve at uniform steps. A cubic's derivative is a
/// quadratic in `t` (`C'(t)/3 = qc + t·qb + t²·qa`), so `C'(t) × offset = 0` is a plain quadratic
/// equation: at most 2 real roots, so a curved edge never needs more than 3 bands.
fn tangent_crossings(p0: P, p1: P, p2: P, p3: P, offset: P) -> Vec<f64> {
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

/// The band(s) between edge `a`(`a_out`) → `b`(`b_in`) and its copy offset by `offset`: one
/// straight-sided band for a straight edge. For a curved one, one band per piece between
/// [`tangent_crossings`] (at most 2, so at most 3 bands) — the curve's true shape on both sides,
/// not a polygon approximation of it, split only where the sweep's own direction actually changes.
fn segment_bands(a: P, a_out: Option<P>, b: P, b_in: Option<P>, offset: P) -> Vec<OutSubpath> {
    if a_out.is_none() && b_in.is_none() {
        return vec![band(a, None, b, None, offset)];
    }
    let (p1, p2) = (a_out.unwrap_or(a), b_in.unwrap_or(b));
    let mut ts = vec![0.0];
    ts.extend(tangent_crossings(a, p1, p2, b, offset));
    ts.push(1.0);
    ts.windows(2)
        .map(|w| {
            let [q0, q1, q2, q3] = sub_cubic(a, p1, p2, b, w[0], w[1]);
            band(q0, Some(q1), q3, Some(q2), offset)
        })
        .collect()
}

fn lerp(a: P, b: P, t: f64) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// De Casteljau split of the cubic bezier `p0 p1 p2 p3` at `t`: the control points of `[0, t]` and
/// `[t, 1]`.
fn split_cubic(p0: P, p1: P, p2: P, p3: P, t: f64) -> ([P; 4], [P; 4]) {
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
fn sub_cubic(p0: P, p1: P, p2: P, p3: P, t0: f64, t1: f64) -> [P; 4] {
    let (_, from_t0) = split_cubic(p0, p1, p2, p3, t0);
    let span = (1.0 - t0).max(1e-9);
    let (upto_t1, _) = split_cubic(from_t0[0], from_t0[1], from_t0[2], from_t0[3], ((t1 - t0) / span).clamp(0.0, 1.0));
    upto_t1
}

/// The closed, 4-anchor band between edge `a`(`a_out`) → `b`(`b_in`) and its copy offset by
/// `offset`: the original curve, a straight connector, the same curve reversed (offset), and a
/// straight connector back.
fn band(a: P, a_out: Option<P>, b: P, b_in: Option<P>, offset: P) -> OutSubpath {
    let (a2, b2) = (add(a, offset), add(b, offset));
    let anchors = vec![
        out_anchor(a, None, a_out),
        out_anchor(b, b_in, None),
        out_anchor(b2, None, b_in.map(|h| add(h, offset))),
        out_anchor(a2, a_out.map(|h| add(h, offset)), None),
    ];
    OutSubpath { anchors, closed: true }
}

fn shift_subpath(sp: &Value, offset: P) -> OutSubpath {
    let closed = sp.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let anchors: Vec<OutAnchor> = sp.get("anchors").and_then(Value::as_array).into_iter().flatten().map(|a| shift_anchor(a, offset)).collect();
    OutSubpath { anchors, closed }
}

/// [`shift_subpath`], but shifting a subpath already built as an [`OutSubpath`] (e.g. a back face)
/// instead of input JSON: used to extrude the back face again for the shadow.
fn shift_out_subpath(sp: &OutSubpath, offset: P) -> OutSubpath {
    let shift = |p: [f64; 2]| [p[0] + offset.0, p[1] + offset.1];
    let anchors = sp.anchors.iter().map(|a| OutAnchor { p: shift(a.p), h_in: a.h_in.map(shift), out: a.out.map(shift) }).collect();
    OutSubpath { anchors, closed: sp.closed }
}

fn shift_anchor(a: &Value, offset: P) -> OutAnchor {
    let p = point(a, "p").unwrap_or((0.0, 0.0));
    out_anchor(add(p, offset), point(a, "in").map(|h| add(h, offset)), point(a, "out").map(|h| add(h, offset)))
}

/// `v[key]` as `(x, y)`; absent for `"in"`/`"out"` means no handle.
fn point(v: &Value, key: &str) -> Option<P> {
    let a = v.get(key)?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// The average position of a band's anchors, projected on the unit offset direction `dir` (for
/// back-to-front sorting).
fn projection(sp: &OutSubpath, dir: P) -> f64 {
    if sp.anchors.is_empty() {
        return 0.0;
    }
    let (sx, sy) = sp.anchors.iter().fold((0.0, 0.0), |(sx, sy), a| (sx + a.p[0], sy + a.p[1]));
    let n = sp.anchors.len() as f64;
    (sx / n) * dir.0 + (sy / n) * dir.1
}

/// The object's top fill, else its top stroke's paint, else a neutral grey: what Extrude & Bevel
/// paints its faces with too.
fn top_paint(obj: &Value) -> Value {
    if let Some(p) = obj.get("fills").and_then(Value::as_array).into_iter().flatten().rev().find(|v| !v.is_null()) {
        return p.clone();
    }
    if let Some(p) = obj.get("strokes").and_then(Value::as_array).into_iter().flatten().rev().find_map(|s| s.get("paint").filter(|p| !p.is_null())) {
        return p.clone();
    }
    json!({"type": "solid", "color": {"model": "gray", "k": 0.6}})
}

/// Every colour (an object with a `model`) inside `v`, moved `amt` of the way to black, in each
/// colour's own model (mirrors the Desaturate example's colour walk). There is at most one paint
/// per output object, so walking it as a `Value` costs nothing like the anchors do.
fn darkened(v: &Value, amt: f64) -> Value {
    let mut v = v.clone();
    darken(&mut v, amt);
    v
}

fn darken(v: &mut Value, amt: f64) {
    match v {
        Value::Object(o) => {
            let model = o.get("model").and_then(Value::as_str).map(str::to_string);
            let get = |key: &str, o: &serde_json::Map<String, Value>| o.get(key).and_then(Value::as_f64).unwrap_or(0.0);
            match model.as_deref() {
                Some("rgb") => {
                    for key in ["r", "g", "b"] {
                        let x = get(key, o);
                        o.insert(key.into(), json!(x * (1.0 - amt)));
                    }
                }
                Some("gray" | "cmyk") => {
                    let k = get("k", o);
                    o.insert("k".into(), json!(k + (1.0 - k) * amt));
                }
                Some("lab") => {
                    let l = get("l", o);
                    o.insert("l".into(), json!(l * (1.0 - amt)));
                }
                _ => o.values_mut().for_each(|c| darken(c, amt)),
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|c| darken(c, amt)),
        _ => {}
    }
}

fn pack(bytes: &[u8]) -> u64 {
    ((bytes.len() as u64) << 32) | bytes.as_ptr() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
        json!({"objects": [{"id": 1, "type": "path", "fillRule": "nonzero",
            "fills": [{"type": "solid", "color": {"model": "rgb", "r": 1.0, "g": 0.0, "b": 0.0}}],
            "path": {"subpaths": [{"closed": true, "anchors": [
                {"p": [x0, y0]}, {"p": [x1, y0]}, {"p": [x1, y1]}, {"p": [x0, y1]}]}]}}]})
    }

    fn run_json(doc: &Value, params: &Value) -> Value {
        serde_json::from_slice(&run(doc, params)).unwrap()
    }

    #[test]
    fn no_selection_is_an_error() {
        let out = run_json(&json!({"objects": []}), &json!({}));
        assert!(out["error"].as_str().unwrap().contains("Select"));
    }

    #[test]
    fn a_square_produces_a_back_copy_four_bands_and_keeps_the_front() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 45, "distance": 40}));
        let objects = out["objects"].as_array().unwrap();
        // back + 4 bands + the front (kept by id).
        assert_eq!(objects.len(), 6, "{out}");
        assert_eq!(objects.last().unwrap(), &json!({"id": 1}), "the front object is kept, unchanged, on top");
        for band in &objects[1..5] {
            assert_eq!(band["type"], "path");
            assert_eq!(band["path"]["subpaths"][0]["anchors"].as_array().unwrap().len(), 4, "{band}");
            assert!(band["path"]["subpaths"][0]["closed"].as_bool().unwrap());
        }
    }

    #[test]
    fn the_back_copy_is_offset_by_angle_and_distance() {
        // Compass convention: 90° is due East, so every point's x grows by the distance and y is
        // unchanged.
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 90, "distance": 40}));
        let back = &out["objects"][0];
        let anchors = back["path"]["subpaths"][0]["anchors"].as_array().unwrap();
        assert_eq!(anchors[0]["p"], json!([40.0, 0.0]));
        assert_eq!(anchors[2]["p"], json!([140.0, 100.0]));
    }

    #[test]
    fn the_compass_angle_wraps_at_360_and_matches_the_cardinal_points() {
        let close = |a: P, b: P| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(close(compass_offset(0.0, 10.0), (0.0, -10.0)), "0°/360°: North, up");
        assert!(close(compass_offset(90.0, 10.0), (10.0, 0.0)), "90°: East, right");
        assert!(close(compass_offset(180.0, 10.0), (0.0, 10.0)), "180°: South, down");
        assert!(close(compass_offset(270.0, 10.0), (-10.0, 0.0)), "270°: West, left");
        // Past either end of the dial, it wraps back onto it instead of clamping.
        assert!(close(compass_offset(360.0, 10.0), compass_offset(0.0, 10.0)), "360° is 0° again");
        assert!(close(compass_offset(450.0, 10.0), compass_offset(90.0, 10.0)), "450° wraps to 90°");
        assert!(close(compass_offset(-90.0, 10.0), compass_offset(270.0, 10.0)), "-90° wraps to 270°");
    }

    #[test]
    fn a_band_connects_the_front_edge_to_its_offset_copy() {
        let bands = bands_for_subpath(&json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}]}), (5.0, -5.0));
        assert_eq!(bands.len(), 2, "a 2-anchor closed subpath has two edges (there and back)");
        let first = &bands[0].anchors;
        assert_eq!(first[0].p, [0.0, 0.0]);
        assert_eq!(first[1].p, [10.0, 0.0]);
        assert_eq!(first[2].p, [15.0, -5.0]);
        assert_eq!(first[3].p, [5.0, -5.0]);
    }

    #[test]
    fn tangent_crossings_are_found_exactly_not_approximated() {
        // A symmetric "S": tangent points straight up at t=0, straight down at t=1, so by symmetry
        // it must be exactly horizontal at the midpoint — and nowhere else.
        let (p0, p1, p2, p3) = ((0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0));
        assert_eq!(tangent_crossings(p0, p1, p2, p3, (1.0, 0.0)), vec![0.5], "horizontal offset: crosses at the midpoint");
        // A vertical offset's two roots land exactly on the endpoints (the curve starts and ends
        // vertical): strictly outside (0, 1), so excluded, not rounded in.
        assert_eq!(tangent_crossings(p0, p1, p2, p3, (0.0, 1.0)), Vec::<f64>::new(), "vertical offset: no interior crossing");
    }

    /// A curved edge whose tangent direction never matches the offset keeps the curve's own
    /// handles exactly: one band for the whole edge, not a chain of small polygon-ish pieces.
    #[test]
    fn a_curved_edge_with_no_tangent_crossing_stays_a_single_band() {
        let sp = json!({"closed": false, "anchors": [
            {"p": [0.0, 0.0], "out": [10.0, 0.0]},
            {"p": [20.0, 0.0], "in": [15.0, 0.0]}]});
        let bands = bands_for_subpath(&sp, (0.0, -30.0));
        assert_eq!(bands.len(), 1, "{bands:?}");
        assert_eq!(bands[0].anchors[0].p, [0.0, 0.0]);
        assert_eq!(bands[0].anchors[0].out, Some([10.0, 0.0]), "the curve's own handle, kept exactly");
        assert_eq!(bands[0].anchors[1].p, [20.0, 0.0]);
        assert_eq!(bands[0].anchors[1].h_in, Some([15.0, 0.0]));
    }

    /// A curve whose tangent crosses the offset direction twice splits into exactly 3 bands, at
    /// exactly those two points — not an arbitrary number of uniform slices.
    #[test]
    fn a_curve_that_turns_twice_relative_to_the_offset_splits_at_its_two_crossings() {
        let (p0, p1, p2, p3) = ((0.0, 0.0), (0.75, 1.0), (-0.5, 2.0), (0.25, 3.0));
        let offset = (0.0, 1.0);
        let crossings = tangent_crossings(p0, p1, p2, p3, offset);
        assert_eq!(crossings.len(), 2, "{crossings:?}");
        assert!((crossings[0] - 0.25).abs() < 1e-9 && (crossings[1] - 0.75).abs() < 1e-9, "{crossings:?}");

        let sp = json!({"closed": false, "anchors": [
            {"p": [p0.0, p0.1], "out": [p1.0, p1.1]},
            {"p": [p3.0, p3.1], "in": [p2.0, p2.1]}]});
        let bands = bands_for_subpath(&sp, offset);
        assert_eq!(bands.len(), 3, "{bands:?}");
        assert_eq!(bands[0].anchors[0].p, [p0.0, p0.1], "the first band starts where the curve does");
        assert_eq!(bands[2].anchors[1].p, [p3.0, p3.1], "the last band ends where the curve does");
        // Seamless: one band's front end is the next's front start.
        for pair in bands.windows(2) {
            let (end, start) = (pair[0].anchors[1].p, pair[1].anchors[0].p);
            assert!((end[0] - start[0]).abs() < 1e-9 && (end[1] - start[1]).abs() < 1e-9, "{end:?} vs {start:?}");
        }
    }

    #[test]
    fn a_straight_edge_stays_a_single_band() {
        let sp = json!({"closed": false, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}]});
        assert_eq!(bands_for_subpath(&sp, (0.0, -30.0)).len(), 1);
    }

    #[test]
    fn with_several_letters_every_depth_face_stays_below_every_front_face() {
        let letter = |id: u64, x: f64| {
            json!({"id": id, "type": "path", "fillRule": "nonzero",
                "fills": [{"type": "solid", "color": {"model": "rgb", "r": 0.0, "g": 0.0, "b": 0.0}}],
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 10.0, 0.0]}, {"p": [x + 10.0, 10.0]}, {"p": [x, 10.0]}]}]}})
        };
        // Two letters typed left to right, extruded towards the previous one (negative x): without
        // a global ordering, the second letter's depth would land on top of the first letter's face.
        let out = run_json(&json!({"objects": [letter(1, 0.0), letter(2, 20.0)]}), &json!({"angle": 180, "distance": 15}));
        let objects = out["objects"].as_array().unwrap();
        let front_ids: Vec<u64> = objects.iter().filter_map(|o| o.get("id")).filter_map(Value::as_u64).collect();
        assert_eq!(front_ids, [1, 2], "both front faces are kept, in their original order, last (on top)");
        // Every object before the last two (the fronts) is depth (a back face or a band): none of
        // them carries an "id", since they're new objects.
        assert!(objects[..objects.len() - 2].iter().all(|o| o.get("id").is_none()));
    }

    #[test]
    fn the_shadow_is_off_by_default() {
        let out = run_json(&square(0.0, 0.0, 10.0, 10.0), &json!({"angle": 225, "distance": 10}));
        // back + 4 bands + the front, nothing extra at the bottom.
        assert_eq!(out["objects"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn the_shadow_extrudes_the_back_face_again_and_merges_it_into_one_shape() {
        let letter = |id: u64, x: f64| {
            json!({"id": id, "type": "path", "fillRule": "nonzero",
                "fills": [{"type": "solid", "color": {"model": "rgb", "r": 0.0, "g": 0.0, "b": 0.0}}],
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 10.0, 0.0]}, {"p": [x + 10.0, 10.0]}, {"p": [x, 10.0]}]}]}})
        };
        // 90°: due East, so the back face sits 15 pt to the right; 180°: due South, so the
        // shadow's own extrusion then sits a further 5 pt straight below that back face.
        let out = run_json(
            &json!({"objects": [letter(1, 0.0), letter(2, 20.0)]}),
            &json!({"angle": 90, "distance": 15, "sideShade": 60, "shadow": true, "shadowAngle": 180, "shadowDistance": 5}),
        );
        let objects = out["objects"].as_array().unwrap();
        // One merged shadow, then both letters' depth and fronts (2 * (back + 4 bands + front)).
        assert_eq!(objects.len(), 1 + 2 * 6, "{out}");
        let shadow = &objects[0];
        assert_eq!(shadow["type"], "compound", "every letter's back face (and its own extrusion) merged into one shape");
        let subpaths = shadow["path"]["subpaths"].as_array().unwrap();
        // Per letter: the back face extruded again = 4 bands + its own back face + the (kept
        // position) back face itself, merged; two letters makes 12.
        assert_eq!(subpaths.len(), 12, "{shadow}");
        assert_eq!(shadow["fills"][0]["color"], json!({"model": "gray", "k": 0.6}), "as dark as sideShade (60%)");
        let bounds = |sp: &Value| {
            let pts: Vec<(f64, f64)> = sp["anchors"].as_array().unwrap().iter().map(|a| point(a, "p").unwrap()).collect();
            let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
            let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
            (x0, y0, x1, y1)
        };
        // Every piece of the first letter's shadow fits inside the Minkowski-sum envelope of its
        // back face (East by 15) extruded straight South by 5 more: 15..25 × 0..15.
        for sp in &subpaths[..6] {
            let (x0, y0, x1, y1) = bounds(sp);
            assert!(x0 >= 15.0 - 1e-9 && x1 <= 25.0 + 1e-9 && y0 >= 0.0 - 1e-9 && y1 <= 15.0 + 1e-9, "{sp}");
        }
        assert!(objects[0].get("id").is_none(), "a new object, not one of the kept letters");
    }

    /// A letter like "O", "A", "B" or "P" has a counter ("contre-forme": its hole, wound the
    /// other way against the much larger outer ring). The shadow must include it too, as a real
    /// hole cut into the single merged shape, not fill over it.
    #[test]
    fn the_shadow_includes_counters_as_real_holes() {
        let outer = json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}, {"p": [20.0, 20.0]}, {"p": [0.0, 20.0]}]});
        let hole = json!({"closed": true, "anchors": [{"p": [5.0, 5.0]}, {"p": [5.0, 15.0]}, {"p": [15.0, 15.0]}, {"p": [15.0, 5.0]}]});
        let obj = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [outer, hole]}});
        // A shadow angle not aligned with any of the square's own axis-aligned edges: otherwise an
        // edge exactly parallel to the offset sweeps into a zero-area (degenerate) band, whose
        // meaningless signed area would otherwise skew the sign count checked below.
        let out =
            run_json(&json!({"objects": [obj]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}));
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        // Outer: 4 bands + its own back face + the (kept position) back face = 6; same for the
        // counter, extruded into the shadow exactly like the outer: 12 total, not 6.
        assert_eq!(subpaths.len(), 12, "the counter must still cast its own shadow pieces: {subpaths:?}");
        let area_of = |sp: &Value| -> f64 {
            let anchors = sp["anchors"].as_array().unwrap();
            let n = anchors.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (point(&anchors[i], "p").unwrap(), point(&anchors[(i + 1) % n], "p").unwrap());
                    a.0 * b.1 - b.0 * a.1
                })
                .sum()
        };
        let signs: Vec<f64> = subpaths.iter().map(|sp| area_of(sp).signum()).collect();
        let (positive, negative): (Vec<f64>, Vec<f64>) = signs.iter().copied().partition(|&s| s > 0.0);
        assert_eq!(
            (positive.len(), negative.len()),
            (6, 6),
            "the outer's 6 pieces and the counter's 6 must wind oppositely so the counter stays a hole: {signs:?}"
        );
    }

    /// Two overlapping letters (no counters here): their outer silhouettes must union seamlessly
    /// into the merged shadow — every one of their pieces shares the same winding, so nonzero
    /// never cancels anything at the shared, overlapping boundary.
    #[test]
    fn the_shadow_merges_overlapping_letters_without_seams() {
        let letter = |id: u64, x: f64| {
            json!({"id": id, "type": "path", "fillRule": "nonzero",
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 20.0, 0.0]}, {"p": [x + 20.0, 20.0]}, {"p": [x, 20.0]}]}]}})
        };
        // The second letter starts well inside the first one: their footprints overlap.
        let out = run_json(
            &json!({"objects": [letter(1, 0.0), letter(2, 10.0)]}),
            &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}),
        );
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        assert_eq!(subpaths.len(), 12, "6 pieces per overlapping letter, none dropped: {subpaths:?}");
        let area_of = |sp: &Value| -> f64 {
            let anchors = sp["anchors"].as_array().unwrap();
            let n = anchors.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (point(&anchors[i], "p").unwrap(), point(&anchors[(i + 1) % n], "p").unwrap());
                    a.0 * b.1 - b.0 * a.1
                })
                .sum()
        };
        let signs: Vec<f64> = subpaths.iter().map(|sp| area_of(sp).signum()).collect();
        assert!(signs.iter().all(|&s| s == signs[0]), "every piece must share one winding so the overlap reads as one seamless union: {signs:?}");
    }

    /// Several letters selected together, each with its own counter: every letterform piece
    /// (across every letter) shares one winding, and every counter piece shares the opposite one —
    /// nothing is lost or filtered, and each letter's own counter stays its own transparent hole.
    #[test]
    fn the_shadow_keeps_each_letters_own_counter_transparent_with_several_letters_selected() {
        let letter_with_counter = |id: u64, x: f64| {
            let outer = json!({"closed": true, "anchors": [
                {"p": [x, 0.0]}, {"p": [x + 20.0, 0.0]}, {"p": [x + 20.0, 20.0]}, {"p": [x, 20.0]}]});
            let hole = json!({"closed": true, "anchors": [
                {"p": [x + 5.0, 5.0]}, {"p": [x + 5.0, 15.0]}, {"p": [x + 15.0, 15.0]}, {"p": [x + 15.0, 5.0]}]});
            json!({"id": id, "type": "compound", "fillRule": "nonzero", "path": {"subpaths": [outer, hole]}})
        };
        // Far enough apart that only their shadows (never their own footprints) could ever touch.
        let out = run_json(
            &json!({"objects": [letter_with_counter(1, 0.0), letter_with_counter(2, 40.0)]}),
            &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}),
        );
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        assert_eq!(subpaths.len(), 24, "12 pieces per letter-with-counter, both letters kept: {subpaths:?}");
        let area_of = |sp: &Value| -> f64 {
            let anchors = sp["anchors"].as_array().unwrap();
            let n = anchors.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (point(&anchors[i], "p").unwrap(), point(&anchors[(i + 1) % n], "p").unwrap());
                    a.0 * b.1 - b.0 * a.1
                })
                .sum()
        };
        let signs: Vec<f64> = subpaths.iter().map(|sp| area_of(sp).signum()).collect();
        let (positive, negative): (Vec<f64>, Vec<f64>) = signs.iter().copied().partition(|&s| s > 0.0);
        assert_eq!((positive.len(), negative.len()), (12, 12), "both letters' outers agree, both counters agree, and oppose: {signs:?}");
    }

    /// Every subpath the shadow merges winds the same way. A band wound the other way from the
    /// back face it comes from would cancel that face's own fill out under nonzero wherever they
    /// overlap, leaving the shadow with no colour there — this is what made it look uncoloured.
    #[test]
    fn every_subpath_of_the_shadow_winds_the_same_way() {
        let letter = |x: f64| {
            json!({"id": 1, "type": "path", "fillRule": "nonzero",
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 10.0, 0.0]}, {"p": [x + 10.0, 10.0]}, {"p": [x, 10.0]}]}]}})
        };
        let out = run_json(
            &json!({"objects": [letter(0.0)]}),
            &json!({"angle": 45, "distance": 15, "shadow": true, "shadowAngle": 225, "shadowDistance": 10}),
        );
        let shadow_subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        let area_of = |sp: &Value| -> f64 {
            let anchors = sp["anchors"].as_array().unwrap();
            let n = anchors.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (point(&anchors[i], "p").unwrap(), point(&anchors[(i + 1) % n], "p").unwrap());
                    a.0 * b.1 - b.0 * a.1
                })
                .sum()
        };
        let signs: Vec<f64> = shadow_subpaths.iter().map(|sp| area_of(sp).signum()).collect();
        assert!(signs.iter().all(|&s| s == signs[0]), "{signs:?}");
    }

    /// An "L": a concave (reflex) corner where the vertical and horizontal strokes meet. Unlike a
    /// scheme that tries to find one clean swept boundary analytically, building a band per edge
    /// and separately fixing up each one's own winding never depends on the shape being convex —
    /// it must stay transparency-free (every piece wound the same way) here too.
    #[test]
    fn a_concave_letterform_still_winds_consistently() {
        let l = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": true, "anchors": [
                {"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}, {"p": [10.0, 30.0]}, {"p": [40.0, 30.0]},
                {"p": [40.0, 40.0]}, {"p": [0.0, 40.0]}]}]}});
        let out = run_json(&json!({"objects": [l]}), &json!({"angle": 45, "distance": 15, "shadow": true, "shadowAngle": 225, "shadowDistance": 12}));
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        let area_of = |sp: &Value| -> f64 {
            let anchors = sp["anchors"].as_array().unwrap();
            let n = anchors.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (point(&anchors[i], "p").unwrap(), point(&anchors[(i + 1) % n], "p").unwrap());
                    a.0 * b.1 - b.0 * a.1
                })
                .sum()
        };
        let signs: Vec<f64> = subpaths.iter().map(area_of).map(f64::signum).collect();
        assert!(signs.iter().all(|&s| s == signs[0]), "{signs:?}");
    }

    /// An open (unclosed) subpath — some fonts' outlines import this way. The shadow must still
    /// extrude it (as many bands as it has segments, none wrapping around to close it), not
    /// silently skip it.
    #[test]
    fn an_open_subpath_still_casts_a_shadow() {
        let open = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": false, "anchors": [
                {"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}, {"p": [10.0, 10.0]}]}]}});
        let out =
            run_json(&json!({"objects": [open]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 180, "shadowDistance": 5}));
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        // 2 segments (open, so no closing edge) = 2 bands, plus its own back face and the back
        // face itself.
        assert_eq!(subpaths.len(), 4, "{subpaths:?}");
    }

    #[test]
    fn side_shade_is_clamped_between_0_and_100_percent() {
        let no_paint = |side_shade| {
            let obj = json!({"id": 1, "type": "path", "fillRule": "nonzero",
                "path": {"subpaths": [{"closed": true, "anchors": [{"p": [0.0,0.0]},{"p": [10.0,0.0]},{"p": [10.0,10.0]},{"p": [0.0,10.0]}]}]}});
            run_json(&json!({"objects": [obj]}), &json!({"angle": 90, "distance": 10, "sideShade": side_shade}))
        };
        // No fill or stroke: the neutral grey fallback (k: 0.6) is the base colour `darken` shades
        // from. Below 0% or above 100%, sideShade clamps to that range instead of pushing the
        // colour past white or past black.
        assert_eq!(no_paint(-50)["objects"][1]["fills"][0]["color"]["k"], json!(0.6), "0%: unshaded, the base grey");
        assert_eq!(no_paint(150)["objects"][1]["fills"][0]["color"]["k"], json!(1.0), "100%: fully black, not darker");
    }

    #[test]
    fn darken_pushes_every_colour_model_towards_black_without_touching_structure() {
        let paint = json!({"type": "gradient", "gradient": {"kind": "Linear", "stops": [
            {"offset": 0.0, "color": {"model": "rgb", "r": 1.0, "g": 0.5, "b": 0.0}},
            {"offset": 1.0, "color": {"model": "cmyk", "c": 0.2, "m": 0.0, "y": 0.0, "k": 0.0}}]}});
        let out = darkened(&paint, 0.5);
        assert_eq!(out["type"], "gradient");
        let stops = &out["gradient"]["stops"];
        assert_eq!(stops[0]["color"]["r"], json!(0.5));
        assert_eq!(stops[1]["color"]["k"], json!(0.5));
        assert_eq!(darkened(&paint, 0.0), paint, "no shading leaves colours untouched");
    }

    #[test]
    fn with_no_paint_a_neutral_grey_is_used() {
        let obj = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": true, "anchors": [{"p": [0.0,0.0]},{"p": [10.0,0.0]},{"p": [10.0,10.0]},{"p": [0.0,10.0]}]}]}});
        let out = run_json(&json!({"objects": [obj]}), &json!({"angle": 45, "distance": 10}));
        assert_eq!(out["objects"][0]["fills"][0]["color"]["model"], "gray");
    }

    /// A round, holed letterform similar to "O"; curved on every edge.
    fn letter_o(cx: f64, cy: f64) -> Value {
        fn circle(cx: f64, cy: f64, r: f64) -> Value {
            let k = 0.5522847498;
            json!({"closed": true, "anchors": [
                {"p": [cx, cy - r], "in": [cx - k * r, cy - r], "out": [cx + k * r, cy - r], "kind": "Smooth"},
                {"p": [cx + r, cy], "in": [cx + r, cy - k * r], "out": [cx + r, cy + k * r], "kind": "Smooth"},
                {"p": [cx, cy + r], "in": [cx + k * r, cy + r], "out": [cx - k * r, cy + r], "kind": "Smooth"},
                {"p": [cx - r, cy], "in": [cx - r, cy + k * r], "out": [cx - r, cy - k * r], "kind": "Smooth"}]})
        }
        json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "fills": [{"type": "solid", "color": {"model": "rgb", "r": 0.1, "g": 0.1, "b": 0.1}}],
            "path": {"subpaths": [circle(cx, cy, 80.0), circle(cx, cy, 40.0)]}})
    }

    /// A letter "O" (an outer ring and an inner hole, both curved) used to exceed the plug-in's
    /// instruction budget: building thousands of anchors as `serde_json::Value` (one `String`
    /// allocation per field) was too costly for the sandbox's interpreter. It must now run in a
    /// few milliseconds, native.
    #[test]
    fn a_curvy_compound_letter_stays_well_inside_the_instruction_budget() {
        let bytes = run(&json!({"objects": [letter_o(100.0, 100.0)]}), &json!({"angle": 45, "distance": 40, "sideShade": 35}));
        let out: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(out.get("error").is_none(), "{out}");
        // A circle's tangent is parallel to any given direction at exactly 2 of its points
        // (diametrically opposite), landing in 2 of its 4 quarter-arc edges (1 extra band each);
        // the other 2 edges keep their single band: 6 bands per circle, 12 for the 2 (outer +
        // hole) subpaths of "O", plus its one merged back face and the kept front.
        assert_eq!(out["objects"].as_array().unwrap().len(), 12 + 2, "{out}");
    }

    /// A full sentence's worth of round, holed letters (the worst realistic case: 8 curved edges
    /// each) used to exceed the budget before the struct-based rewrite. Each curved edge now splits
    /// only where its own tangent actually crosses the offset direction (at most 2 points, so at
    /// most 3 bands), a bound that holds per edge regardless of how many letters are selected: no
    /// adaptive scaling is needed any more to keep a long selection inside the budget.
    #[test]
    fn a_long_selection_of_curvy_letters_still_fits_the_instruction_budget() {
        let objects: Vec<Value> = (0..40)
            .map(|i| {
                let mut o = letter_o(100.0 + i as f64 * 180.0, 100.0);
                o["id"] = json!(i);
                o
            })
            .collect();
        let bytes = run(&json!({"objects": objects}), &json!({"angle": 45, "distance": 40, "sideShade": 35}));
        let out: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(out.get("error").is_none(), "{out}");
        // Each of the 40 letters produces the same fixed count as a single one above: the total
        // grows linearly with the selection, never risking the budget however long it gets.
        assert_eq!(out["objects"].as_array().unwrap().len(), 40 * 14, "{out}");
    }

    /// The shadow re-extrudes every back face, roughly doubling the work for the same selection —
    /// still unconditionally safe, since each curved edge's band count is bounded on its own.
    #[test]
    fn a_long_selection_still_fits_the_budget_with_the_shadow_on_too() {
        let objects: Vec<Value> = (0..40)
            .map(|i| {
                let mut o = letter_o(100.0 + i as f64 * 180.0, 100.0);
                o["id"] = json!(i);
                o
            })
            .collect();
        let bytes = run(
            &json!({"objects": objects}),
            &json!({"angle": 45, "distance": 40, "sideShade": 35, "shadow": true, "shadowAngle": 180, "shadowDistance": 20}),
        );
        let out: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(out.get("error").is_none(), "{out}");
    }
}
