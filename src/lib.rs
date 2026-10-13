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
//! JSON bytes (never through a `serde_json::Value` tree): with curves split into several bands
//! each (see [`CURVE_STEPS`]), a single letter can need thousands of anchors, and building each one
//! as a `Value` (a `String`-keyed map, allocated per field) costs enough interpreted instructions
//! in the plug-in sandbox to exceed its budget. Only `serde` and `serde_json` are used: no imports,
//! no WASI.

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

    // The curve subdivision (see `curve_steps`) that keeps the whole selection's total band count,
    // and so the plug-in's instruction budget, bounded regardless of how many letters are selected.
    // With the shadow on, the back face is extruded a second time: half the budget for each pass
    // keeps the total about the same as the extrusion alone.
    let steps = curve_steps(&objects, if shadow_on { 2 } else { 1 });
    let valid: Vec<&Value> = objects.iter().filter(|o| o.get("path").is_some()).collect();

    // The cast shadow (one merged shape, the back face of every letter extruded again towards the
    // shadow's own angle and distance) goes first: under the depth and the front faces, at the
    // very back of the whole selection.
    let mut result = vec![];
    if shadow_on {
        let shadow_angle = params["shadowAngle"].as_f64().unwrap_or(115.0);
        let shadow_distance = params["shadowDistance"].as_f64().unwrap_or(50.0).clamp(1.0, 2000.0);
        result.extend(shadow_object(&valid, offset, compass_offset(shadow_angle, shadow_distance), steps, side_amt));
    }

    // Every letter's depth (back face + side bands) is stacked below every letter's front face, in
    // that order, whichever object comes first: with several letters (e.g. a word typed left to
    // right), one letter's depth must never cover a neighbour's front face.
    let (mut depth, mut fronts) = (vec![], vec![]);
    for obj in &valid {
        let (back_and_bands, front) = extrude_one(obj, offset, side_amt, steps);
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

/// The cast shadow: an extrusion of the main back face (kept exactly where it already is, not the
/// original front letterform), towards its own `shadow_offset`, by the same principle as the main
/// extrusion — a second back face and the bands connecting the two. Every letter's back face, its
/// second back face and its bands are merged into one compound object (so the whole shadow is a
/// single shape, not one per letter), filled with a flat grey as dark as `side_amt` (the side
/// bands' own shading). `None` when nothing in `objects` has geometry to cast one.
fn shadow_object(objects: &[&Value], main_offset: P, shadow_offset: P, steps: usize, side_amt: f64) -> Option<OutItem> {
    let mut subpaths = vec![];
    for sp in objects.iter().flat_map(|o| o["path"]["subpaths"].as_array().into_iter().flatten()) {
        let back = shift_subpath(sp, main_offset);
        // Depending on which edge they come from, bands can wind either way against their own
        // contour (half its edges go "with" the offset, half "against"): fine for the main
        // extrusion, where each band is its own object, but merged into one shape with one shared
        // fillRule, an oppositely wound band cancels the contour's own fill out under it instead
        // of adding to it — the shadow would then show no colour wherever that happens.
        let sign = signed_area(&back);
        subpaths.extend(bands_for_out_subpath(&back, shadow_offset, steps).into_iter().map(|b| wound_like(b, sign)));
        subpaths.push(shift_out_subpath(&back, shadow_offset));
        subpaths.push(back);
    }
    if subpaths.is_empty() {
        return None;
    }
    // The same grey intensity as the side bands (sideShade), not the letters' own colour.
    let fill = json!({"type": "solid", "color": {"model": "gray", "k": side_amt}});
    Some(OutItem::New(NewObj { kind: "compound", path: OutPathData { subpaths }, fill_rule: "nonzero", fills: [fill] }))
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
fn extrude_one(obj: &Value, offset: P, side_amt: f64, steps: usize) -> (Vec<OutItem>, Option<OutItem>) {
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
        subpaths.iter().flat_map(|sp| bands_for_subpath(sp, offset, steps)).map(|sp| (projection(&sp, dir), sp)).collect();
    // Furthest along the offset direction (most hidden) first, so later bands and the front face
    // layer correctly on top.
    bands.sort_by(|a, b| b.0.total_cmp(&a.0));
    result.extend(bands.into_iter().map(|(_, sp)| {
        OutItem::New(NewObj { kind: "path", path: OutPathData { subpaths: vec![sp] }, fill_rule: "nonzero", fills: [side_fill.clone()] })
    }));

    (result, obj.get("id").map(|id| OutItem::Kept { id: id.clone() }))
}

/// One quadrilateral (or `steps` curved bands, when the edge is a curve) per boundary edge of
/// `sp`, filling the gap between it and its copy offset by `offset`.
fn bands_for_subpath(sp: &Value, offset: P, steps: usize) -> Vec<OutSubpath> {
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
            Some(segment_bands(point(a, "p")?, point(a, "out"), point(b, "p")?, point(b, "in"), offset, steps))
        })
        .flatten()
        .collect()
}

/// The same as [`bands_for_subpath`], but reading a subpath already built as an [`OutSubpath`]
/// (e.g. a back face) instead of input JSON: used to extrude the back face again for the shadow.
fn bands_for_out_subpath(sp: &OutSubpath, offset: P, steps: usize) -> Vec<OutSubpath> {
    let n = sp.anchors.len();
    if n < 2 {
        return vec![];
    }
    let segments = if sp.closed { n } else { n - 1 };
    (0..segments)
        .filter_map(|i| {
            let a = sp.anchors.get(i % n)?;
            let b = sp.anchors.get((i + 1) % n)?;
            Some(segment_bands((a.p[0], a.p[1]), a.out.map(|h| (h[0], h[1])), (b.p[0], b.p[1]), b.h_in.map(|h| (h[0], h[1])), offset, steps))
        })
        .flatten()
        .collect()
}

/// The most bands one curved edge may be split into (the best case: one letter, a few curves),
/// and the fewest (the worst case: a long selection), so a curve is still visibly better than the
/// original single straight-sided band that started this.
const MAX_CURVE_STEPS: usize = 12;
const MIN_CURVE_STEPS: usize = 2;
/// Roughly how many bands the whole selection should produce: the plug-in sandbox's instruction
/// budget is one flat amount per run, not one per selected object, so a selection with many curved
/// letters (a long word, a sentence) must subdivide each curve less to stay inside it. Chosen with
/// a comfortable margin under the budget measured on a worst-case round, holed letterform.
const TARGET_TOTAL_BANDS: usize = 320;

/// The number of bands ([`MIN_CURVE_STEPS`]..=[`MAX_CURVE_STEPS`]) to split every curved edge into
/// for this run, so the selection's total band count stays near [`TARGET_TOTAL_BANDS`] ÷ `passes`
/// however many letters (and however many curves) are selected: more curves selected at once, or
/// more extrusion passes (the shadow re-extrudes the back face, so `passes` is 2 with it on),
/// fewer steps each.
fn curve_steps(objects: &[Value], passes: usize) -> usize {
    let (mut curved, mut straight) = (0usize, 0usize);
    for sp in objects.iter().flat_map(|o| o["path"]["subpaths"].as_array().into_iter().flatten()) {
        let (c, s) = segment_kinds(sp);
        curved += c;
        straight += s;
    }
    (TARGET_TOTAL_BANDS / passes.max(1))
        .saturating_sub(straight)
        .checked_div(curved)
        .unwrap_or(MAX_CURVE_STEPS)
        .clamp(MIN_CURVE_STEPS, MAX_CURVE_STEPS)
}

/// How many of a subpath's edges are curves (at least one handle) versus straight, without
/// building any bands yet: cheap enough to size [`curve_steps`] for the whole selection first.
fn segment_kinds(sp: &Value) -> (usize, usize) {
    let anchors = sp.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default();
    let closed = sp.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let n = anchors.len();
    if n < 2 {
        return (0, 0);
    }
    let segments = if closed { n } else { n - 1 };
    let (mut curved, mut straight) = (0, 0);
    for i in 0..segments {
        let (Some(a), Some(b)) = (anchors.get(i % n), anchors.get((i + 1) % n)) else { continue };
        if point(a, "out").is_some() || point(b, "in").is_some() {
            curved += 1
        } else {
            straight += 1
        }
    }
    (curved, straight)
}

/// The band(s) between edge `a`(`a_out`) → `b`(`b_in`) and its copy offset by `offset`: one
/// straight-sided band for a straight edge, or `steps` bands following the curve exactly (each a
/// piece of the same cubic, so neighbouring bands share the true tangent) for a curved one.
fn segment_bands(a: P, a_out: Option<P>, b: P, b_in: Option<P>, offset: P, steps: usize) -> Vec<OutSubpath> {
    if a_out.is_none() && b_in.is_none() {
        return vec![band(a, None, b, None, offset)];
    }
    let (p1, p2) = (a_out.unwrap_or(a), b_in.unwrap_or(b));
    (0..steps)
        .map(|i| {
            let t0 = i as f64 / steps as f64;
            let t1 = (i + 1) as f64 / steps as f64;
            let [q0, q1, q2, q3] = sub_cubic(a, p1, p2, b, t0, t1);
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
        let bands = bands_for_subpath(&json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}]}), (5.0, -5.0), MAX_CURVE_STEPS);
        assert_eq!(bands.len(), 2, "a 2-anchor closed subpath has two edges (there and back)");
        let first = &bands[0].anchors;
        assert_eq!(first[0].p, [0.0, 0.0]);
        assert_eq!(first[1].p, [10.0, 0.0]);
        assert_eq!(first[2].p, [15.0, -5.0]);
        assert_eq!(first[3].p, [5.0, -5.0]);
    }

    #[test]
    fn a_curved_edge_is_split_into_bands_that_follow_its_own_tangent() {
        let sp = json!({"closed": false, "anchors": [
            {"p": [0.0, 0.0], "out": [10.0, 0.0]},
            {"p": [20.0, 0.0], "in": [15.0, 0.0]}]});
        let steps = 7;
        let bands = bands_for_subpath(&sp, (0.0, -30.0), steps);
        // Connecting the two anchors directly (a single straight-sided band) cuts the curve's
        // corner; following its tangent takes several smaller bands instead.
        assert_eq!(bands.len(), steps);
        assert_eq!(bands[0].anchors[0].p, [0.0, 0.0], "the first band starts where the curve does");
        assert_eq!(bands[steps - 1].anchors[1].p, [20.0, 0.0], "the last band ends where the curve does");
        // Consecutive bands are seamless: one's front end is the next's front start (allowing for
        // last-bit floating point rounding between the two different De Casteljau subdivisions).
        for pair in bands.windows(2) {
            let (end, start) = (pair[0].anchors[1].p, pair[1].anchors[0].p);
            assert!((end[0] - start[0]).abs() < 1e-9 && (end[1] - start[1]).abs() < 1e-9, "{end:?} vs {start:?}");
        }
        // Every band's front curve ends exactly where the original cubic is at that parameter (De
        // Casteljau subdivision reconstructs it, not an approximation).
        for (i, band) in bands.iter().enumerate() {
            let t = (i as f64 + 1.0) / steps as f64;
            let [_, _, _, expected] = sub_cubic((0.0, 0.0), (10.0, 0.0), (15.0, 0.0), (20.0, 0.0), 0.0, t);
            let got = band.anchors[1].p;
            assert!((got[0] - expected.0).abs() < 1e-9 && (got[1] - expected.1).abs() < 1e-9, "band {i}: {got:?} vs {expected:?}");
        }
    }

    #[test]
    fn a_straight_edge_stays_a_single_band() {
        let sp = json!({"closed": false, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}]});
        assert_eq!(bands_for_subpath(&sp, (0.0, -30.0), MAX_CURVE_STEPS).len(), 1);
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
        // subpaths[4]: the back face's own back face (further South); subpaths[5]: the back face
        // itself, kept exactly where the main extrusion already put it (East by the distance).
        assert_eq!(subpaths[5]["anchors"][0]["p"], json!([15.0, 0.0]), "the back face, unmoved");
        assert_eq!(subpaths[4]["anchors"][0]["p"], json!([15.0, 5.0]), "extruded again, straight South");
        assert!(objects[0].get("id").is_none(), "a new object, not one of the kept letters");
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
        // One letter's curves get the full MAX_CURVE_STEPS: 2 subpaths * 4 curved edges * steps
        // bands, plus the back face and the front kept.
        assert_eq!(out["objects"].as_array().unwrap().len(), 2 * 4 * MAX_CURVE_STEPS + 2);
    }

    /// A full sentence's worth of round, holed letters (the worst realistic case: 8 curved edges
    /// each) used to exceed the budget even after the struct-based rewrite above — at a flat
    /// `MAX_CURVE_STEPS`, the total band count grows with the selection, not just with one
    /// letter's own curves. `curve_steps` must scale it down so a long selection still succeeds.
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
        assert_eq!(out["objects"].as_array().unwrap().len(), 40 * (2 * 4 * MIN_CURVE_STEPS + 2));
    }

    /// The shadow re-extrudes every back face, roughly doubling the work for the same selection:
    /// `curve_steps` must halve its budget per pass when it is on, or a long selection that was
    /// fine without it could exceed the instruction budget with it.
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
