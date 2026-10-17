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
  "description": "Duplicates the selected letterforms, offsets the copy by an angle and distance, and builds the connecting side faces as separate, selectable objects: the classic hand-lettering fake-perspective extrusion. At a diagonal angle, only the two faces the extrusion actually reveals are kept (e.g. South and West at 225°), each following its own boundary's curvature and split into its own cavalier-perspective tone.",
  "params": {
    "angle": {"type": "number", "min": 0, "max": 360, "default": 225},
    "distance": {"type": "number", "min": 1, "max": 2000, "default": 40},
    "sideShade": {"type": "number", "min": 0, "max": 100, "default": 35},
    "sideContrast": {"type": "number", "min": 0, "max": 100, "default": 30},
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

/// The two cardinal directions bracketing `angle_deg` (`d1`, then `d2 = d1 + 90°`): the two
/// families of faces a cavalier-perspective extrusion at that angle shows on its lateral surface
/// (e.g. 225° brackets to South, then West). `None` at an exact cardinal angle (0/90/180/270°):
/// the whole lateral surface is then a single face, no split needed.
fn face_family(angle_deg: f64) -> Option<(f64, f64)> {
    let q = angle_deg.rem_euclid(360.0) / 90.0;
    if (q - q.round()).abs() < 1e-9 {
        return None;
    }
    let d1 = 90.0 * q.floor();
    Some((d1, (d1 + 90.0).rem_euclid(360.0)))
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
    let contrast = (params["sideContrast"].as_f64().unwrap_or(30.0) / 100.0).clamp(0.0, 1.0);
    let offset = compass_offset(angle, distance);
    let shadow_on = params["shadow"].as_bool().unwrap_or(false);
    let valid: Vec<&Value> = objects.iter().filter(|o| o.get("path").is_some()).collect();

    // The cast shadow (one merged shape, every letter's outer silhouette — never its counters —
    // extruded again towards the shadow's own angle and distance) goes first: under the depth and
    // the front faces, at the very back of the whole selection.
    let mut result = vec![];
    let shadow_angle = params["shadowAngle"].as_f64().unwrap_or(115.0);
    if shadow_on {
        let shadow_distance = params["shadowDistance"].as_f64().unwrap_or(50.0).clamp(1.0, 2000.0);
        result.extend(shadow_object(&valid, offset, compass_offset(shadow_angle, shadow_distance), side_amt));
    }

    // Every letter's depth (back face + side bands) is stacked below every letter's front face:
    // with several letters (e.g. a word typed left to right), one letter's depth must never cover
    // a neighbour's front face. Within that, each letter's own back stays behind its own bands,
    // which are sorted by how far along the offset direction they sit: a band positioned further
    // IN that direction (e.g. towards the bottom-left at 225°) is physically nearer the viewer in
    // this fake-perspective technique, hence drawn LAST (on top) of one further against it (e.g.
    // towards the top-right there), not the other way around — this is what keeps overlapping
    // bands at a concave corner (e.g. where a letter's leg meets its stem) layering in the right
    // order, exactly like the back face vs. the front face already do (back is 100% against the
    // offset direction, front is 100% towards the viewer, and every band sits somewhere on that
    // same single depth axis in between). The LETTERS themselves are bucketed in an order that
    // depends on which way the offset leans horizontally: an eastward-leaning extrusion (its own
    // distance pushed rightward, `offset.0 >= 0`) pushes each letter's depth into the NEXT
    // letter's space, so the natural typing order already puts an earlier letter's depth behind a
    // later one, as wanted. A westward-leaning one (`offset.0 < 0`) pushes depth the other way,
    // into the PREVIOUS letter's space instead — reaching backwards over text already laid down —
    // so that encroaching depth must stay behind the earlier letter it reaches into, meaning the
    // bucket order has to run the other way: the LAST letter's depth is the most hidden, the
    // FIRST letter's the least.
    // The cast shadow stands in for the scene's own light direction (the light sits opposite the
    // shadow, so the pair of visible faces nearest the shadow's OWN direction are the ones turned
    // away from the light): `dark_reference` is that shadow angle when there is one, else a fixed
    // default assuming an overhead light (South/down stays dark, matching ordinary lettering) —
    // see [`face_tone`].
    let dark_reference = if shadow_on { shadow_angle } else { 180.0 };
    let mut per_letter: Vec<(Option<OutItem>, Vec<(f64, OutItem)>, Option<OutItem>)> =
        valid.iter().map(|obj| extrude_one(obj, offset, side_amt, contrast, angle, dark_reference)).collect();
    let fronts: Vec<OutItem> = per_letter.iter_mut().filter_map(|(_, _, front)| front.take()).collect();
    if offset.0 < 0.0 {
        per_letter.reverse();
    }
    let (mut backs, mut bands) = (vec![], vec![]);
    for (back, mut letter_bands, _) in per_letter {
        backs.extend(back);
        letter_bands.sort_by(|a, b| a.0.total_cmp(&b.0));
        bands.extend(letter_bands.into_iter().map(|(_, item)| item));
    }
    if result.is_empty() && backs.is_empty() && bands.is_empty() && fronts.is_empty() {
        return NO_SELECTION.to_vec();
    }
    result.extend(backs);
    result.extend(bands);
    result.extend(fronts);
    serde_json::to_vec(&Output { objects: result }).unwrap_or_else(|_| NO_SELECTION.to_vec())
}

/// The cast shadow: every letter's own back face (outer contour AND counters alike — "contre-
/// formes", the eye of an "e", the hole of an "O", "A" or "B" — nothing is excluded) kept exactly
/// where the main back face already put it, extruded again towards `shadow_offset` by the same
/// principle as the main extrusion — a second back face and the bands connecting the two. Every
/// piece, from every letter, is merged into ONE compound object: normalizing every letterform
/// piece to one reference winding, and every counter piece to the OPPOSITE one, makes several
/// overlapping letters' outer silhouettes read as one seamless union (no seam at their shared
/// boundary) while each counter keeps subtracting — staying transparent — exactly like the
/// original glyph's own fill rule already does (this is as far as "Pathfinder Unite" goes here:
/// the plug-in sandbox has no access to the host's own boolean engine, and working per edge like
/// this never depends on the shape being convex, closed, or free of the sharp interior angles some
/// fonts' outlines have). Filled with a flat grey as dark as `side_amt` (the side bands' own
/// shading, never two-toned: the cast shadow is a flat silhouette, not a cavalier-perspective
/// solid). `None` when nothing in `objects` has geometry to cast one.
///
/// A counter's own connecting bands (the wall of its own little tube, from its hole boundary here
/// to its shadow-shifted copy) are always solid material, never part of the hole — exactly how the
/// main extrusion already treats a hole's own side bands (the inside wall of an "O" is shaded,
/// same as its outer wall; only the FRONT and BACK faces actually show through the hole). Only the
/// two flat cross-sections — the counter's shape kept here, and its shadow-shifted copy further
/// out — stay the counter's own transparent hole.
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
        // Bands are always solid (`true`), regardless of `is_outer`: see the doc comment above.
        subpaths.extend(bands_for_out_subpath(&back, shadow_offset).into_iter().map(|b| (wound_like(b, sign), true)));
        subpaths.push((shift_out_subpath(&back, shadow_offset), is_outer));
        subpaths.push((back, is_outer));
    }
    if subpaths.is_empty() {
        return None;
    }
    // Every solid piece (across every letter, letterform or band alike) shares one winding, so
    // they simply union together; every counter's own flat cross-section shares the OPPOSITE one,
    // so it keeps subtracting from whatever solid material it overlaps (its own letter's, almost
    // always).
    let reference = subpaths.iter().find(|(_, is_outer)| *is_outer).map(|(sp, _)| signed_area(sp)).unwrap_or(1.0);
    let subpaths: Vec<OutSubpath> =
        subpaths.into_iter().map(|(sp, is_outer)| wound_like(sp, if is_outer { reference } else { -reference })).collect();
    // The same grey intensity as the side bands (sideShade), not the letters' own colour.
    let fill = json!({"type": "solid", "color": {"model": "gray", "k": side_amt}});
    Some(OutItem::New(NewObj { kind: "compound", path: OutPathData { subpaths }, fill_rule: "nonzero", fills: [fill] }))
}

/// `obj`'s subpaths, each tagged with whether it's letterform (`true`, e.g. both the dot and the
/// stem of "i", which share no counter) or one of its counters (`false`, e.g. the hole of "O",
/// "A", "B" or "P"): the largest one by area is always letterform; every other one is a counter
/// if it sits GEOMETRICALLY INSIDE it, decided by a point-in-polygon test on its own centroid, not
/// by comparing winding signs. Winding sign alone isn't reliable here: some glyph sources draw a
/// hole with the OPPOSITE winding from its outer ring (so plain nonzero fill already shows it as a
/// hole), but others draw it with the SAME winding and rely on `evenodd` fill instead — sign
/// comparison would wrongly tag every subpath as letterform for that second kind of data (nothing
/// would ever be detected as a counter, so nothing would ever stay transparent). Containment is a
/// geometric fact, true either way.
fn subpaths_with_role(obj: &Value) -> Vec<(Value, bool)> {
    let Some(subpaths) = obj["path"]["subpaths"].as_array() else { return vec![] };
    let Some(dominant) = subpaths.iter().enumerate().max_by(|(_, a), (_, b)| signed_area_json(a).abs().total_cmp(&signed_area_json(b).abs())) else {
        return vec![];
    };
    let (dominant_i, dominant_anchors) = (dominant.0, dominant.1.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default());
    subpaths
        .iter()
        .enumerate()
        .map(|(i, sp)| {
            let is_counter =
                i != dominant_i && sp.get("anchors").and_then(Value::as_array).is_some_and(|a| point_in_polygon(centroid(a), &dominant_anchors));
            (sp.clone(), !is_counter)
        })
        .collect()
}

/// The average of `anchors`' own points (ignoring curvature): a representative point known to sit
/// well inside a typical (roughly convex, as real letterforms' counters are) closed subpath, for
/// [`point_in_polygon`] to test against another one.
fn centroid(anchors: &[Value]) -> P {
    let (sx, sy, n) = anchors.iter().filter_map(|a| point(a, "p")).fold((0.0, 0.0, 0.0_f64), |(sx, sy, n), p| (sx + p.0, sy + p.1, n + 1.0));
    if n == 0.0 {
        (0.0, 0.0)
    } else {
        (sx / n, sy / n)
    }
}

/// Whether `p` lies inside the closed polygon formed by `anchors`' own points (ignoring curvature,
/// the same approximation [`signed_area_json`] already makes): a standard ray-casting test, used
/// to tell a counter from a separate letterform piece regardless of which way either one winds.
fn point_in_polygon(p: P, anchors: &[Value]) -> bool {
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

/// The back duplicate, the side bands (each tagged with its own projection along the offset
/// direction for a GLOBAL back-to-front sort across every letter's bands, see [`run`]), and the
/// unchanged front object. At a diagonal `angle`, the bands split into two cavalier-perspective
/// tones (see [`face_family`]): `side_amt` plus or minus half of `contrast`, assigned by
/// `dark_reference` (see [`face_tone`]).
fn extrude_one(
    obj: &Value,
    offset: P,
    side_amt: f64,
    contrast: f64,
    angle: f64,
    dark_reference: f64,
) -> (Option<OutItem>, Vec<(f64, OutItem)>, Option<OutItem>) {
    let Some(subpaths) = obj["path"]["subpaths"].as_array() else { return (None, vec![], None) };
    let rule: &'static str = if obj["fillRule"].as_str() == Some("evenodd") { "evenodd" } else { "nonzero" };
    let kind: &'static str = if obj["type"].as_str() == Some("compound") { "compound" } else { "path" };
    let paint = top_paint(obj);
    let side_fill = darkened(&paint, side_amt);
    let face = face_family(angle);
    let tone_fills =
        face.map(|_| (darkened(&paint, (side_amt + contrast / 2.0).clamp(0.0, 1.0)), darkened(&paint, (side_amt - contrast / 2.0).clamp(0.0, 1.0))));
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
        .flat_map(|sp| bands_for_subpath(sp, offset, face, dominant_sign, angle, dark_reference))
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
fn bands_for_subpath(
    sp: &Value,
    offset: P,
    face: Option<(f64, f64)>,
    dominant_sign: f64,
    angle: f64,
    dark_reference: f64,
) -> Vec<(OutSubpath, Option<bool>)> {
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
        face_tone((pb.0 - pa.0, pb.1 - pa.1), dominant_sign, face, angle, dark_reference).is_none()
    };
    bridge_runs(segments, closed, is_dropped_straight)
        .into_iter()
        .flat_map(|piece| match piece {
            BandPiece::Bridge(i0, i1) => {
                let (Some(a), Some(b)) = (anchors.get(i0 % n).and_then(|a| point(a, "p")), anchors.get(i1 % n).and_then(|a| point(a, "p"))) else {
                    return vec![];
                };
                let Some((d1, d2)) = face else { return vec![] };
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
                segment_bands(pa, point(a, "out"), pb, point(b, "in"), offset, face, dominant_sign, angle, dark_reference)
            }
        })
        .collect()
}

/// Whether convex quads `q1`/`q2` (each 4 points, in order) overlap in AREA, by the separating
/// axis theorem: they don't if some axis perpendicular to one of the 8 edges has their two
/// projections only touching or apart, with a small tolerance so two quads that are merely
/// adjacent (sharing a full edge, as two directly kept neighbours legitimately do: see
/// [`bands_for_subpath`]'s doc comment) read as NOT overlapping, even right at a shared VERTEX
/// (where a plain point-in-polygon test on the raw corners is ambiguous) — good enough to decide
/// whether a bridge is needed, not a general-purpose exact intersection test.
fn quads_overlap(q1: &[P; 4], q2: &[P; 4]) -> bool {
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

/// A normal boundary edge, processed as usual, or a bridge replacing a run of dropped ones: see
/// [`bridge_runs`].
enum BandPiece {
    Edge(usize),
    Bridge(usize, usize),
}

/// Splits `segments` consecutive boundary edges into normal per-edge pieces and "bridges": a
/// bridge replaces a MAXIMAL RUN of 1+ consecutive edges `is_dropped` reports as fully hidden,
/// from the run's own first vertex straight to its last, forced to stay visible (never itself
/// dropped) — see [`bands_for_subpath`]'s doc comment for why. Vertex indices in a returned
/// `Bridge` may be `>= segments` (wrapping around a closed subpath): take them `% segments` (or
/// `% anchors.len()`) before indexing.
fn bridge_runs(segments: usize, closed: bool, is_dropped: impl Fn(usize) -> bool) -> Vec<BandPiece> {
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

/// The same as [`bands_for_subpath`], but reading a subpath already built as an [`OutSubpath`]
/// (e.g. a back face) instead of input JSON: used to extrude the back face again for the shadow,
/// which stays a single uniform tone and is never pruned (always passes `face: None`).
fn bands_for_out_subpath(sp: &OutSubpath, offset: P) -> Vec<OutSubpath> {
    let n = sp.anchors.len();
    if n < 2 {
        return vec![];
    }
    let sign = signed_area(sp);
    let segments = if sp.closed { n } else { n - 1 };
    (0..segments)
        .filter_map(|i| {
            let a = sp.anchors.get(i % n)?;
            let b = sp.anchors.get((i + 1) % n)?;
            Some(segment_bands(
                (a.p[0], a.p[1]),
                a.out.map(|h| (h[0], h[1])),
                (b.p[0], b.p[1]),
                b.h_in.map(|h| (h[0], h[1])),
                offset,
                None,
                sign,
                0.0,
                0.0,
            ))
        })
        .flatten()
        .map(|(sp, _)| sp)
        .collect()
}

/// The 2D cross product (scalar): positive when `b` is counter-clockwise from `a`.
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

/// `v`'s own compass bearing (0°=North/up, 90°=East/right, clockwise), in `[0, 360)` — the
/// inverse of [`compass_offset`]'s direction.
fn bearing(v: P) -> f64 {
    v.0.atan2(-v.1).to_degrees().rem_euclid(360.0)
}

/// The shortest distance between two bearings (`0..=180°`).
fn circ_dist360(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
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
fn outward_bearing(tangent: P, dominant_sign: f64) -> f64 {
    (bearing(tangent) - 90.0 * dominant_sign.signum()).rem_euclid(360.0)
}

/// `None` to drop this boundary piece entirely: its outward direction is more than 90° away from
/// the extrusion's own `angle` (true back-face-culling: an edge is only ever revealed if the
/// offset pushes at least partly "into" the gap it would open up) — keeping only what's actually
/// shown lightens the output as well as the work. Else the tone to paint the kept piece with:
/// `None` for a single uniform tone when `face` is `None` itself (an exact cardinal angle: nothing
/// is pruned then, the whole lateral surface is one face), else `Some(true)` for whichever of
/// [`face_family`]'s `d1`/`d2` sits closer to `dark_reference` (darker) or `Some(false)` for the
/// other (lighter) — a coarser, fixed quadrant-based split, only used to pick a colour for a piece
/// already known to be visible, never to decide visibility itself (unlike the drop test above,
/// THIS part is deliberately quantized to 2 fixed cardinal tones, not continuously following
/// `angle`, so a letter's two visible sides each stay one flat colour as the user sweeps the angle
/// within a quadrant). `dark_reference` is the cast shadow's own angle when there is one (the
/// shadow stands in for the scene's light, from the opposite direction: the face nearest the
/// shadow's own direction is the one turned away from that light, hence darker) or a fixed default
/// (due South) otherwise, so lettering with no shadow still reads as lit from a consistent,
/// plausible direction instead of an arbitrary one. `dominant_sign`: see [`outward_bearing`].
fn face_tone(tangent: P, dominant_sign: f64, face: Option<(f64, f64)>, angle: f64, dark_reference: f64) -> Option<Option<bool>> {
    let Some((d1, d2)) = face else { return Some(None) };
    let normal = outward_bearing(tangent, dominant_sign);
    if circ_dist360(normal, angle) > 90.0 {
        return None;
    }
    let d1_is_darker = circ_dist360(d1, dark_reference) <= circ_dist360(d2, dark_reference);
    let (dark, light) = if d1_is_darker { (d1, d2) } else { (d2, d1) };
    Some(Some(circ_dist360(normal, dark) <= circ_dist360(normal, light)))
}

/// The cubic bezier `p0 p1 p2 p3`'s own tangent direction at `t` (unnormalized): `B'(t) =
/// 3(1-t)²(p1-p0) + 6(1-t)t(p2-p1) + 3t²(p3-p2)`.
fn cubic_tangent(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
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
fn segment_bands(
    a: P,
    a_out: Option<P>,
    b: P,
    b_in: Option<P>,
    offset: P,
    face: Option<(f64, f64)>,
    sign: f64,
    angle: f64,
    dark_reference: f64,
) -> Vec<(OutSubpath, Option<bool>)> {
    if a_out.is_none() && b_in.is_none() {
        let Some(tone) = face_tone((b.0 - a.0, b.1 - a.1), sign, face, angle, dark_reference) else { return vec![] };
        return vec![(band(a, None, b, None, offset), tone)];
    }
    let (p1, p2) = (a_out.unwrap_or(a), b_in.unwrap_or(b));
    let mut ts = vec![0.0];
    ts.extend(tangent_crossings(a, p1, p2, b, offset));
    if let Some((d1, _)) = face {
        let unit = {
            let len = (offset.0 * offset.0 + offset.1 * offset.1).sqrt().max(1e-9);
            (offset.0 / len, offset.1 / len)
        };
        // Skip when it's the offset's own line (e.g. the default 225°, already a diagonal):
        // already covered by the crossings just added above.
        let dir = compass_offset(d1 + 45.0, 1.0);
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
            let tone = face_tone(cubic_tangent(a, p1, p2, b, (w[0] + w[1]) / 2.0), sign, face, angle, dark_reference)?;
            Some((band(q0, Some(q1), q3, Some(q2), offset), tone))
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

    /// `face_tone`'s drop/keep decision must agree, for EVERY combination of extrusion angle and
    /// edge bearing (not just the few hand-picked ones elsewhere in this file), with the true,
    /// independent "back-face culling" test: a piece is genuinely visible only if sweeping its own
    /// edge by the offset adds REAL area on the outward side, i.e. `cross(tangent, offset) *
    /// dominant_sign < 0` (hand-derived and cross-checked against concrete N/E/S/W examples in
    /// `outward_bearing`'s own doc comment). This is the regression test for a real user report:
    /// just past a cardinal angle (e.g. 91°), an edge whose own outward bearing was still within
    /// 90° of the TRUE angle (e.g. a diagonal stroke near the top of a letter) used to be wrongly
    /// dropped because the old test compared against the coarse, fixed quadrant bisector (`d1 +
    /// 45°`) instead of the continuously moving `angle` itself.
    #[test]
    fn face_tone_agrees_with_true_back_face_culling_at_every_angle_and_bearing() {
        for angle_millideg in (0..360_000).step_by(1297) {
            let angle = angle_millideg as f64 / 1000.0;
            // Skip the exact cardinals: `face_family` deliberately keeps everything there with a
            // single uniform tone (a benign simplification, since the pieces it keeps beyond what
            // the true test would are all degenerate/zero-area anyway), not meant to match the
            // true per-edge test below.
            let Some(face) = face_family(angle) else { continue };
            let offset = compass_offset(angle, 10.0);
            for bearing_millideg in (0..360_000).step_by(2063) {
                let edge_bearing = bearing_millideg as f64 / 1000.0;
                for sign in [1.0, -1.0] {
                    let tangent = compass_offset((edge_bearing + 90.0 * sign).rem_euclid(360.0), 1.0);
                    // More than a tiny margin away from the boundary: exactly at it, the swept
                    // area is genuinely ~zero and either answer is visually meaningless, not a
                    // real disagreement.
                    if (circ_dist360(outward_bearing(tangent, sign), angle) - 90.0).abs() < 0.5 {
                        continue;
                    }
                    let kept = face_tone(tangent, sign, Some(face), angle, 180.0).is_some();
                    let true_kept = cross(tangent, offset) * sign < 0.0;
                    assert_eq!(
                        kept, true_kept,
                        "angle={angle} edge_bearing={edge_bearing} sign={sign}: face_tone kept={kept} but true back-face test says {true_kept}"
                    );
                }
            }
        }
    }

    /// A letterform like a bold "R": a vertical stem with a thin diagonal leg bulging out of its
    /// right edge, rejoining it further down (a reflex vertex, as a real "R"'s leg has where it
    /// meets the stem). Dropping the stem's own hidden edge between them independently used to
    /// leave the leg's own inner band and the stem's own bottom band no longer literally adjacent,
    /// letting their two swept quads cross — a visible overlap right at the join, as reported from
    /// a real render. The dropped run is now bridged by one extra band instead, correctly filling
    /// in the gap itself (this is checked, not just assumed).
    ///
    /// Known limitation (not fixed by this, and not checked by this test): a long or steep enough
    /// edge's own swept quad can independently reach other, non-adjacent geometry regardless of
    /// what sits between them, since this extrusion sweeps every edge by the SAME fixed vector
    /// rather than each one's own outward normal clipped against its neighbours (unlike a standard
    /// variable-width stroke offset) — bridging the gap restores the two kept bands' own direct
    /// adjacency, but can't shrink either one's own already-oversized natural extent. Properly
    /// fixing that in general needs clipping every pair of adjacent bands against each other, a
    /// deeper change than this one.
    #[test]
    fn a_reflex_notch_between_two_kept_edges_is_bridged() {
        let r = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": true, "anchors": [
                {"p": [0.0, 0.0]}, {"p": [120.0, 0.0]}, {"p": [120.0, 250.0]},
                {"p": [330.0, 450.0]}, {"p": [280.0, 480.0]}, {"p": [120.0, 350.0]},
                {"p": [120.0, 480.0]}, {"p": [0.0, 480.0]}]}]}});
        let out = run_json(&json!({"objects": [r]}), &json!({"angle": 225, "distance": 120, "sideShade": 35, "sideContrast": 30}));
        let objects = out["objects"].as_array().unwrap();
        // back + the usual 4 kept edges (stem's bottom and left, leg's inner and foot) + the
        // bridge over the stem's own hidden edge between the leg and the stem's bottom + the
        // front: one more than before this fix.
        assert_eq!(objects.len(), 7, "{out}");
        let bridge = objects.iter().find(|o| {
            o["path"]["subpaths"][0]["anchors"].as_array().is_some_and(|a| {
                a.len() == 4 && a.iter().any(|p| point(p, "p") == Some((120.0, 350.0))) && a.iter().any(|p| point(p, "p") == Some((120.0, 480.0)))
            })
        });
        assert!(bridge.is_some(), "the notch between the leg's inner edge and the stem's bottom edge must be bridged: {out}");
    }

    /// Two bands at very different positions (not sharing any vertex, unlike the convex-square
    /// cases elsewhere in this file): the one sitting further IN the offset direction (here, more
    /// towards the bottom-left at 225°: the stem's own bottom band) must be drawn AFTER — on top
    /// of — the one sitting further AGAINST it (more towards the top-right: the leg's own inner
    /// band), matching this fake-perspective technique's own front/back axis (the front face is
    /// 100% against the offset direction and is always on top of everything; the back face is
    /// 100% along it and always underneath; every band sits somewhere on that same axis in
    /// between). Getting this backwards is what let the wrong one win in their overlap at a
    /// concave corner (reported from a real render), even once the overlap itself has a bridge.
    #[test]
    fn a_band_further_along_the_offset_is_drawn_on_top_of_one_further_against_it() {
        let r = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": true, "anchors": [
                {"p": [0.0, 0.0]}, {"p": [120.0, 0.0]}, {"p": [120.0, 250.0]},
                {"p": [330.0, 450.0]}, {"p": [280.0, 480.0]}, {"p": [120.0, 350.0]},
                {"p": [120.0, 480.0]}, {"p": [0.0, 480.0]}]}]}});
        let out = run_json(&json!({"objects": [r]}), &json!({"angle": 225, "distance": 120, "sideShade": 35, "sideContrast": 30}));
        let objects = out["objects"].as_array().unwrap();
        let find_index = |p0: P, p1: P| {
            objects.iter().position(|o| {
                o["path"]["subpaths"][0]["anchors"]
                    .as_array()
                    .is_some_and(|a| a.len() == 4 && a.iter().any(|p| point(p, "p") == Some(p0)) && a.iter().any(|p| point(p, "p") == Some(p1)))
            })
        };
        let leg_inner = find_index((280.0, 480.0), (120.0, 350.0)).unwrap_or_else(|| panic!("{out}"));
        let stem_bottom = find_index((120.0, 480.0), (0.0, 480.0)).unwrap_or_else(|| panic!("{out}"));
        assert!(
            stem_bottom > leg_inner,
            "the stem's bottom band (further towards the bottom-left) must be drawn after (on top of) the leg's inner band: {out}"
        );
    }

    /// An ordinary square (no reflex notch at all) at a diagonal angle still only shows its two
    /// revealed faces, with no spurious bridge connecting the two DROPPED ones on its hidden side
    /// (North and East here) — the fix above must tell a short, genuinely-reflex notch apart from
    /// an everyday long run of hidden edges, never bridging the latter.
    #[test]
    fn an_ordinary_hidden_run_is_not_bridged() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 225, "distance": 40}));
        // back + 2 bands (South, West) + the front: unchanged from before this fix.
        assert_eq!(out["objects"].as_array().unwrap().len(), 4, "{out}");
    }

    #[test]
    fn no_selection_is_an_error() {
        let out = run_json(&json!({"objects": []}), &json!({}));
        assert!(out["error"].as_str().unwrap().contains("Select"));
    }

    #[test]
    fn a_square_produces_a_back_copy_and_only_the_two_faces_the_extrusion_reveals() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 45, "distance": 40}));
        let objects = out["objects"].as_array().unwrap();
        // back + 2 bands (only North/East are revealed at 45°, see face_family) + the front (kept by id).
        assert_eq!(objects.len(), 4, "{out}");
        assert_eq!(objects.last().unwrap(), &json!({"id": 1}), "the front object is kept, unchanged, on top");
        for band in &objects[1..3] {
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
        let bands =
            bands_for_subpath(&json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}]}), (5.0, -5.0), None, 1.0, 0.0, 180.0);
        assert_eq!(bands.len(), 2, "a 2-anchor closed subpath has two edges (there and back)");
        let first = &bands[0].0.anchors;
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
        let bands = bands_for_subpath(&sp, (0.0, -30.0), None, 1.0, 0.0, 180.0);
        assert_eq!(bands.len(), 1, "{bands:?}");
        assert_eq!(bands[0].0.anchors[0].p, [0.0, 0.0]);
        assert_eq!(bands[0].0.anchors[0].out, Some([10.0, 0.0]), "the curve's own handle, kept exactly");
        assert_eq!(bands[0].0.anchors[1].p, [20.0, 0.0]);
        assert_eq!(bands[0].0.anchors[1].h_in, Some([15.0, 0.0]));
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
        let bands = bands_for_subpath(&sp, offset, None, 1.0, 0.0, 180.0);
        assert_eq!(bands.len(), 3, "{bands:?}");
        assert_eq!(bands[0].0.anchors[0].p, [p0.0, p0.1], "the first band starts where the curve does");
        assert_eq!(bands[2].0.anchors[1].p, [p3.0, p3.1], "the last band ends where the curve does");
        // Seamless: one band's front end is the next's front start.
        for pair in bands.windows(2) {
            let (end, start) = (pair[0].0.anchors[1].p, pair[1].0.anchors[0].p);
            assert!((end[0] - start[0]).abs() < 1e-9 && (end[1] - start[1]).abs() < 1e-9, "{end:?} vs {start:?}");
        }
    }

    #[test]
    fn a_straight_edge_stays_a_single_band() {
        let sp = json!({"closed": false, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}]});
        assert_eq!(bands_for_subpath(&sp, (0.0, -30.0), None, 1.0, 0.0, 180.0).len(), 1);
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
        // back + 2 bands (only South/West are revealed at 225deg) + the front, nothing extra.
        assert_eq!(out["objects"].as_array().unwrap().len(), 4);
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

    /// A letter like "O", "A" or "B" has a counter ("contre-forme": its hole, the "œil" of a
    /// letter like "e", wound the other way against the much larger outer ring). The shadow must
    /// include it too, as a real hole cut into the single merged shape, not fill over it.
    #[test]
    fn the_shadow_includes_counters_as_real_holes() {
        let outer = json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}, {"p": [20.0, 20.0]}, {"p": [0.0, 20.0]}]});
        let hole = json!({"closed": true, "anchors": [{"p": [5.0, 5.0]}, {"p": [5.0, 15.0]}, {"p": [15.0, 15.0]}, {"p": [15.0, 5.0]}]});
        let obj = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [outer, hole]}});
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
        // The outer's own 6 pieces, plus the counter's own 4 bands (its tube wall is solid,
        // exactly like the outer's own side bands), share one winding; only the counter's 2 flat
        // cross-sections (its shape here, and its shadow-shifted copy) oppose it and stay a hole.
        assert_eq!(
            (positive.len(), negative.len()),
            (10, 2),
            "only the counter's 2 flat cross-sections stay a hole, not its connecting bands: {signs:?}"
        );
    }

    /// A counter's own connecting bands (the wall of its own little tube, from its shape here to
    /// its shadow-shifted copy) must be solid, not holes — tagging them as holes (same as its two
    /// flat cross-sections) would create a transparent "tunnel" connecting the two cross-sections,
    /// looking like a hollow cylinder cut clean through the grey shadow.
    #[test]
    fn a_counters_connecting_bands_are_solid_not_a_see_through_tunnel() {
        let outer = json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}, {"p": [20.0, 20.0]}, {"p": [0.0, 20.0]}]});
        let hole = json!({"closed": true, "anchors": [{"p": [5.0, 5.0]}, {"p": [5.0, 15.0]}, {"p": [15.0, 15.0]}, {"p": [15.0, 5.0]}]});
        let obj = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [outer, hole]}});
        let (angle, distance, shadow_angle, shadow_distance) = (90.0, 10.0, 135.0, 5.0);
        let out = run_json(
            &json!({"objects": [obj]}),
            &json!({"angle": angle, "distance": distance, "shadow": true, "shadowAngle": shadow_angle, "shadowDistance": shadow_distance}),
        );
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
        // The counter's own two flat cross-sections, computed directly the same way the plug-in
        // itself builds them: its shape at the back face, and that shifted again towards the
        // shadow.
        let main_offset = compass_offset(angle, distance);
        let shadow_offset = compass_offset(shadow_angle, shadow_distance);
        let shift = |p: (f64, f64), o: (f64, f64)| (p.0 + o.0, p.1 + o.1);
        let hole_pts: Vec<(f64, f64)> = hole["anchors"].as_array().unwrap().iter().map(|a| point(a, "p").unwrap()).collect();
        let cap1: Vec<(f64, f64)> = hole_pts.iter().map(|&p| shift(p, main_offset)).collect();
        let cap2: Vec<(f64, f64)> = cap1.iter().map(|&p| shift(p, shadow_offset)).collect();
        let matches_cap = |sp: &Value, cap: &[(f64, f64)]| {
            let anchors = sp["anchors"].as_array().unwrap();
            anchors.len() == cap.len()
                && anchors.iter().all(|a| {
                    let p = point(a, "p").unwrap();
                    cap.iter().any(|c| (c.0 - p.0).abs() < 1e-6 && (c.1 - p.1).abs() < 1e-6)
                })
        };
        let cap1_sp =
            subpaths.iter().find(|sp| matches_cap(sp, &cap1)).unwrap_or_else(|| panic!("the counter's own back face not found: {subpaths:?}"));
        let cap2_sp =
            subpaths.iter().find(|sp| matches_cap(sp, &cap2)).unwrap_or_else(|| panic!("the counter's shadow-shifted copy not found: {subpaths:?}"));
        assert!((area_of(cap1_sp) * area_of(cap2_sp)) > 0.0, "both the counter's cross-sections wind the same way");
        // Every OTHER subpath (the outer's own 6 pieces, and the counter's own 4 connecting
        // bands) must wind the OPPOSITE way: the connecting bands are solid, matching the outer,
        // not holes matching their own counter's cross-sections.
        let cap_sign = area_of(cap1_sp).signum();
        for sp in subpaths {
            if std::ptr::eq(sp, cap1_sp) || std::ptr::eq(sp, cap2_sp) {
                continue;
            }
            assert_ne!(area_of(sp).signum(), cap_sign, "every piece other than the counter's own cross-sections must be solid: {sp}");
        }
    }

    /// Some glyph sources draw a counter with the SAME winding as its outer ring (relying on
    /// `evenodd` fill, not on opposite winding under `nonzero`) — the hole must still be detected
    /// and stay transparent, not disappear entirely. Detecting a counter by comparing winding
    /// signs tags NOTHING as a counter for this kind of data (both subpaths share the same sign),
    /// so the whole shadow would render fully solid grey with no hole at all.
    #[test]
    fn a_same_winding_counter_still_stays_transparent() {
        let outer = json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [20.0, 0.0]}, {"p": [20.0, 20.0]}, {"p": [0.0, 20.0]}]});
        // Wound the SAME way as `outer` (both TL,TR,BR,BL) — only valid as a hole under `evenodd`.
        let hole = json!({"closed": true, "anchors": [{"p": [5.0, 5.0]}, {"p": [15.0, 5.0]}, {"p": [15.0, 15.0]}, {"p": [5.0, 15.0]}]});
        let obj = json!({"id": 1, "type": "path", "fillRule": "evenodd",
            "path": {"subpaths": [outer, hole]}});
        let out =
            run_json(&json!({"objects": [obj]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}));
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
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
            (10, 2),
            "the same-winding counter must still be detected (geometric containment, not sign comparison) and stay a hole: {signs:?}"
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

    /// Several letters selected together, only one of them with a counter: nothing is filtered or
    /// normalized away, every subpath of every letter simply gets extruded into the shadow and
    /// merged — a plain letter's own piece count must stay the same whether or not another letter
    /// in the same selection happens to have a counter.
    #[test]
    fn the_shadow_merges_every_letters_subpaths_without_dropping_any() {
        let plain = json!({"id": 1, "type": "path", "fillRule": "nonzero",
            "path": {"subpaths": [{"closed": true, "anchors": [
                {"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}, {"p": [10.0, 10.0]}, {"p": [0.0, 10.0]}]}]}});
        let outer = json!({"closed": true, "anchors": [{"p": [20.0, 0.0]}, {"p": [40.0, 0.0]}, {"p": [40.0, 20.0]}, {"p": [20.0, 20.0]}]});
        let hole = json!({"closed": true, "anchors": [{"p": [25.0, 5.0]}, {"p": [25.0, 15.0]}, {"p": [35.0, 15.0]}, {"p": [35.0, 5.0]}]});
        let with_counter = json!({"id": 2, "type": "compound", "fillRule": "nonzero", "path": {"subpaths": [outer, hole]}});
        let out = run_json(
            &json!({"objects": [plain, with_counter]}),
            &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}),
        );
        let subpaths = out["objects"][0]["path"]["subpaths"].as_array().unwrap();
        // The plain letter: 6 pieces, same as always; the one with a counter: 12, same as the
        // single-letter case above — nothing lost by selecting them together.
        assert_eq!(subpaths.len(), 6 + 12, "{subpaths:?}");
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

    /// A round, holed letterform similar to "O"; curved on every edge. Both circles happen to
    /// share the same winding (not a valid nonzero hole pair) — fine for the budget tests below,
    /// which only count objects, but see [`a_curved_counters_connecting_bands_are_solid_not_a_see_
    /// through_tunnel`] for a properly opposite-wound "O".
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
        // Only the North/East faces are revealed at 45deg. The old implementation split each
        // circle's curve at its own now-redundant "visibility boundary" line in addition to the
        // offset's own crossings (the two coincide only at the exact bisector in direction, never
        // in which LINE they cut along), producing extra, needlessly-split pieces; removing that
        // redundant split merges adjacent same-tone pieces into fewer, larger bands with the same
        // visible coverage (6 kept, not 8), plus the one merged back face and the kept front.
        assert_eq!(out["objects"].as_array().unwrap().len(), 8, "{out}");
    }

    /// A true, properly-wound "O" (unlike [`letter_o`], whose two circles happen to share the same
    /// winding and so never actually behave like a real nonzero counter): the outer ring CW, the
    /// inner hole CCW, both curved on every edge. The shadow must still treat the hole's own
    /// connecting bands as solid and only its two flat cross-sections as the actual hole, exactly
    /// as for a straight-edged counter.
    #[test]
    fn a_curved_counters_connecting_bands_are_solid_not_a_see_through_tunnel() {
        fn circle(cx: f64, cy: f64, r: f64, clockwise: bool) -> Value {
            let k = 0.5522847498;
            let pts = [
                (cx, cy - r, cx - k * r, cy - r, cx + k * r, cy - r),
                (cx + r, cy, cx + r, cy - k * r, cx + r, cy + k * r),
                (cx, cy + r, cx + k * r, cy + r, cx - k * r, cy + r),
                (cx - r, cy, cx - r, cy + k * r, cx - r, cy - k * r),
            ];
            let mut anchors: Vec<Value> =
                pts.iter().map(|&(px, py, ix, iy, ox, oy)| json!({"p": [px, py], "in": [ix, iy], "out": [ox, oy]})).collect();
            if !clockwise {
                anchors.reverse();
                for a in &mut anchors {
                    let (p_in, p_out) = (a["in"].clone(), a["out"].clone());
                    a["in"] = p_out;
                    a["out"] = p_in;
                }
            }
            json!({"closed": true, "anchors": anchors})
        }
        let obj = json!({"id": 1, "type": "compound", "fillRule": "nonzero",
            "path": {"subpaths": [circle(100.0, 100.0, 80.0, true), circle(100.0, 100.0, 40.0, false)]}});
        let (angle, distance, shadow_angle, shadow_distance) = (90.0, 10.0, 135.0, 5.0);
        let out = run_json(
            &json!({"objects": [obj]}),
            &json!({"angle": angle, "distance": distance, "shadow": true, "shadowAngle": shadow_angle, "shadowDistance": shadow_distance}),
        );
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
        // The inner circle's own two flat cross-sections, computed directly the same way the
        // plug-in itself builds them: its shape at the back face, and that shifted again towards
        // the shadow.
        let hole = circle(100.0, 100.0, 40.0, false);
        let main_offset = compass_offset(angle, distance);
        let shadow_offset = compass_offset(shadow_angle, shadow_distance);
        let shift = |p: (f64, f64), o: (f64, f64)| (p.0 + o.0, p.1 + o.1);
        let hole_pts: Vec<(f64, f64)> = hole["anchors"].as_array().unwrap().iter().map(|a| point(a, "p").unwrap()).collect();
        let cap1: Vec<(f64, f64)> = hole_pts.iter().map(|&p| shift(p, main_offset)).collect();
        let cap2: Vec<(f64, f64)> = cap1.iter().map(|&p| shift(p, shadow_offset)).collect();
        let matches_cap = |sp: &Value, cap: &[(f64, f64)]| {
            let anchors = sp["anchors"].as_array().unwrap();
            anchors.len() == cap.len()
                && anchors.iter().all(|a| {
                    let p = point(a, "p").unwrap();
                    cap.iter().any(|c| (c.0 - p.0).abs() < 1e-6 && (c.1 - p.1).abs() < 1e-6)
                })
        };
        let cap1_sp = subpaths.iter().find(|sp| matches_cap(sp, &cap1)).unwrap_or_else(|| panic!("the hole's own back face not found: {subpaths:?}"));
        let cap2_sp =
            subpaths.iter().find(|sp| matches_cap(sp, &cap2)).unwrap_or_else(|| panic!("the hole's shadow-shifted copy not found: {subpaths:?}"));
        assert!((area_of(cap1_sp) * area_of(cap2_sp)) > 0.0, "both the hole's cross-sections wind the same way");
        let cap_sign = area_of(cap1_sp).signum();
        for sp in subpaths {
            if std::ptr::eq(sp, cap1_sp) || std::ptr::eq(sp, cap2_sp) {
                continue;
            }
            assert_ne!(area_of(sp).signum(), cap_sign, "every piece other than the hole's own cross-sections must be solid: {sp}");
        }
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
        assert_eq!(out["objects"].as_array().unwrap().len(), 40 * 8, "{out}");
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

    #[test]
    fn face_family_brackets_a_diagonal_angle_into_its_two_cardinal_neighbours() {
        assert_eq!(face_family(225.0), Some((180.0, 270.0)), "South then West");
        assert_eq!(face_family(45.0), Some((0.0, 90.0)), "North then East");
        assert_eq!(face_family(135.0), Some((90.0, 180.0)), "East then South");
        assert_eq!(face_family(315.0), Some((270.0, 0.0)), "West then North");
    }

    #[test]
    fn an_exact_cardinal_angle_needs_only_one_face() {
        for angle in [0.0, 90.0, 180.0, 270.0, 360.0, 450.0] {
            assert_eq!(face_family(angle), None, "{angle}° is a single face, no split needed");
        }
    }

    /// A square at the default (diagonal) angle: only its bottom (South, horizontal) and left
    /// (West, vertical) edges are revealed -- face_family's darker d1 and lighter d2 -- while its
    /// top (North) and right (East) edges, facing the pair the extrusion doesn't reveal, are
    /// dropped entirely, regardless of which way the square happens to be wound.
    #[test]
    fn only_the_two_faces_the_extrusion_reveals_are_kept_and_toned() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 225, "distance": 40, "sideShade": 35, "sideContrast": 30}));
        let objects = out["objects"].as_array().unwrap();
        // back + 2 bands (only South/West are revealed at 225deg) + the front.
        assert_eq!(objects.len(), 4, "{out}");
        let is_horizontal = |band: &Value| {
            let a = &band["path"]["subpaths"][0]["anchors"];
            (a[0]["p"][1].as_f64().unwrap() - a[1]["p"][1].as_f64().unwrap()).abs() < 1e-9
        };
        for band in &objects[1..3] {
            let r = band["fills"][0]["color"]["r"].as_f64().unwrap();
            if is_horizontal(band) {
                assert!((r - 0.5).abs() < 1e-9, "South (bottom): sideShade + half of sideContrast, the darker tone 1: {band}");
            } else {
                assert!((r - 0.8).abs() < 1e-9, "West (left): sideShade - half of sideContrast, the lighter tone 2: {band}");
            }
        }
    }

    /// At an exact cardinal angle, the two-tone split stays off: every band keeps the single
    /// `sideShade` tone, exactly as before this feature existed.
    #[test]
    fn a_cardinal_angle_keeps_a_single_uniform_tone() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 90, "distance": 40, "sideShade": 35, "sideContrast": 30}));
        let bands = &out["objects"].as_array().unwrap()[1..5];
        for band in bands {
            assert_eq!(band["fills"][0]["color"]["r"], json!(0.65), "{band}");
        }
    }

    /// The two-tone dark/light split must correlate with the cast shadow's own direction: the
    /// shadow stands in for the scene's light (from the opposite direction), so the visible face
    /// nearest the shadow's own direction is the one turned away from that light, hence darker.
    /// Same extrusion (South-East, so South and East are its two visible faces) in both cases,
    /// only the shadow's own direction differs.
    #[test]
    fn the_dark_tone_sits_on_whichever_visible_face_is_nearest_the_shadow() {
        let bottom_right_shadow = run_json(
            &square(0.0, 0.0, 100.0, 100.0),
            &json!({"angle": 135, "distance": 40, "sideShade": 35, "sideContrast": 30, "shadow": true, "shadowAngle": 160, "shadowDistance": 20}),
        );
        let top_right_shadow = run_json(
            &square(0.0, 0.0, 100.0, 100.0),
            &json!({"angle": 135, "distance": 40, "sideShade": 35, "sideContrast": 30, "shadow": true, "shadowAngle": 20, "shadowDistance": 20}),
        );
        let is_horizontal = |band: &Value| {
            let a = &band["path"]["subpaths"][0]["anchors"];
            (a[0]["p"][1].as_f64().unwrap() - a[1]["p"][1].as_f64().unwrap()).abs() < 1e-9
        };
        for (out, bottom_is_dark, label) in [(&bottom_right_shadow, true, "bottom-right"), (&top_right_shadow, false, "top-right")] {
            // Object 0 is the shadow (merged compound), object 1 the back face: the extrusion's
            // own two bands follow those.
            let bands = &out["objects"].as_array().unwrap()[2..4];
            for band in bands {
                let r = band["fills"][0]["color"]["r"].as_f64().unwrap();
                let expect_dark = is_horizontal(band) == bottom_is_dark;
                let expected = if expect_dark { 0.5 } else { 0.8 };
                assert!((r - expected).abs() < 1e-9, "{label} shadow, {band}: expected r={expected} (dark={expect_dark})");
            }
        }
    }

    /// With no shadow at all, the dark tone defaults to the BOTTOM (South) face when the
    /// extrusion reveals one -- the usual "lit from above" assumption for plain lettering.
    #[test]
    fn with_no_shadow_the_bottom_face_defaults_to_dark() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 135, "distance": 40, "sideShade": 35, "sideContrast": 30}));
        let is_horizontal = |band: &Value| {
            let a = &band["path"]["subpaths"][0]["anchors"];
            (a[0]["p"][1].as_f64().unwrap() - a[1]["p"][1].as_f64().unwrap()).abs() < 1e-9
        };
        for band in &out["objects"].as_array().unwrap()[1..3] {
            let r = band["fills"][0]["color"]["r"].as_f64().unwrap();
            let expected = if is_horizontal(band) { 0.5 } else { 0.8 };
            assert!((r - expected).abs() < 1e-9, "{band}");
        }
    }

    /// With no shadow and an extrusion that pushes UPWARD (so neither visible face is the bottom
    /// one), the default "lit from above" reference has no South face to land on: the dark tone
    /// falls on the SIDE face instead, not on whichever happens to be `d1`.
    #[test]
    fn with_no_shadow_and_an_upward_extrusion_the_side_face_defaults_to_dark() {
        let out = run_json(&square(0.0, 0.0, 100.0, 100.0), &json!({"angle": 45, "distance": 40, "sideShade": 35, "sideContrast": 30}));
        let is_horizontal = |band: &Value| {
            let a = &band["path"]["subpaths"][0]["anchors"];
            (a[0]["p"][1].as_f64().unwrap() - a[1]["p"][1].as_f64().unwrap()).abs() < 1e-9
        };
        for band in &out["objects"].as_array().unwrap()[1..3] {
            let r = band["fills"][0]["color"]["r"].as_f64().unwrap();
            // The horizontal (North, top) face is now the LIGHT one; the vertical (East, side)
            // face is dark -- the opposite mapping from the South-leaning tests above.
            let expected = if is_horizontal(band) { 0.8 } else { 0.5 };
            assert!((r - expected).abs() < 1e-9, "{band}");
        }
    }

    /// A curved edge whose tangent crosses the offset direction (used for the band geometry
    /// itself, at `t=0.5` here, see `tangent_crossings_are_found_exactly_not_approximated`) AND
    /// the tone boundary (two different lines here: at 60°, the offset points East-North-East,
    /// while the tone boundary between its North/East face pair sits at 45°) must split at both,
    /// and drop whichever resulting piece faces more than 90° away from the true extrusion angle.
    #[test]
    fn a_curved_edge_also_splits_at_the_tone_and_visibility_boundaries_and_drops_hidden_pieces() {
        let (p0, p1, p2, p3) = ((0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0));
        let angle = 60.0;
        let bands = segment_bands(p0, Some(p1), p3, Some(p2), compass_offset(angle, 10.0), face_family(angle), 1.0, angle, 180.0);
        assert!(bands.len() < 4, "at least one of the (up to 4) pieces must be dropped as not visible: {bands:?}");
    }

    /// At an angle NOT at its own quadrant's bisector (unlike the project's usual 45°/225° test
    /// angles, where the old quadrant-bisector approximation and the true extrusion angle happen
    /// to coincide), a piece whose outward direction is still within 90° of the TRUE angle must be
    /// kept even though it's more than 90° from the coarse `d1+45°` bisector the old, buggy test
    /// used to compare against instead — this is the exact regression a user reported ("a facet is
    /// missing at the top of the letters just past a cardinal angle").
    #[test]
    fn a_piece_near_the_true_angle_is_kept_even_past_the_coarse_bisector() {
        let angle = 100.0;
        let face = face_family(angle);
        assert_eq!(face, Some((90.0, 180.0)), "East then South");
        // A tangent whose outward direction (with a CCW/positive-signed parent) is 30°: 70° from
        // the true 100° angle (kept, `<= 90°`) but 105° from the old, coarse bisector `d1+45 =
        // 135°` (which the old implementation would have wrongly dropped). `outward_bearing` is
        // `bearing(tangent) - 90°`, so a tangent bearing 120° away gives outward bearing 30°.
        let tangent = compass_offset(120.0, 1.0);
        // `dark_reference` is set to East (`d1`) itself so this test's `true` expectation stays
        // about the VISIBILITY fix only, independent of the separate dark/light correlation rule.
        let tone = face_tone(tangent, 1.0, face, angle, 90.0);
        assert_eq!(tone, Some(Some(true)), "within 90° of the true 100° angle, must stay a kept (darker, closer to East/d1) piece: {tone:?}");
    }

    /// A letter with a counter (hole) like "A", "B" or "O": the hole's own inner walls must still
    /// get bands at a diagonal angle, exactly like the outer boundary's — only half of each are
    /// pruned, never ALL of the hole's (which would wrongly show the background/shadow through the
    /// counter instead of a wall).
    #[test]
    fn a_counter_still_gets_its_own_two_revealed_faces_not_pruned_entirely() {
        let outer = json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [100.0, 0.0]}, {"p": [100.0, 100.0]}, {"p": [0.0, 100.0]}]});
        let hole = json!({"closed": true, "anchors": [{"p": [30.0, 30.0]}, {"p": [30.0, 70.0]}, {"p": [70.0, 70.0]}, {"p": [70.0, 30.0]}]});
        let obj = json!({"id": 1, "type": "compound", "fillRule": "nonzero",
            "fills": [{"type": "solid", "color": {"model": "rgb", "r": 1.0, "g": 0.0, "b": 0.0}}],
            "path": {"subpaths": [outer, hole]}});
        let out = run_json(&json!({"objects": [obj]}), &json!({"angle": 225, "distance": 40}));
        let objects = out["objects"].as_array().unwrap();
        // back (compound, outer+hole) + 2 outer bands + 2 hole bands + the front.
        assert_eq!(objects.len(), 6, "{out}");
        let touches_hole = |band: &Value| {
            band["path"]["subpaths"][0]["anchors"].as_array().unwrap().iter().any(|a| {
                let p = point(a, "p").unwrap();
                (30.0..=70.0).contains(&p.0) && (30.0..=70.0).contains(&p.1)
            })
        };
        assert!(objects[1..5].iter().any(touches_hole), "the hole's own walls must still be present: {out}");
    }

    /// Three letters typed left to right ("ABC"): with the default (westward-leaning) extrusion
    /// angle, each letter's bands must stay behind the one typed before it, not bucketed by typing
    /// order — a westward offset pushes every letter's depth backward into the PREVIOUS letter's
    /// own space (the opposite of an eastward-leaning extrusion, where typing order already works,
    /// since it pushes depth into the NEXT letter's space instead).
    #[test]
    fn a_westward_extrusion_keeps_each_letter_behind_the_one_typed_before_it() {
        let letter = |id: u64, x: f64| {
            json!({"id": id, "type": "path", "fillRule": "nonzero",
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 10.0, 0.0]}, {"p": [x + 10.0, 10.0]}, {"p": [x, 10.0]}]}]}})
        };
        let out = run_json(&json!({"objects": [letter(1, 0.0), letter(2, 10.0), letter(3, 20.0)]}), &json!({"angle": 225, "distance": 15}));
        let objects = out["objects"].as_array().unwrap();
        // A back face or band (never a kept front, which only ever serializes its own "id").
        let is_new = |o: &Value| o.get("id").is_none();
        let bottom_of = |x0: f64| {
            objects
                .iter()
                .position(|o| {
                    is_new(o) && {
                        let a = &o["path"]["subpaths"][0]["anchors"];
                        let (p0, p1) = (point(&a[0], "p").unwrap(), point(&a[1], "p").unwrap());
                        (p0.1 - p1.1).abs() < 1e-9
                            && ((p0.0 - x0).abs() < 1e-9 && (p1.0 - (x0 + 10.0)).abs() < 1e-9
                                || (p1.0 - x0).abs() < 1e-9 && (p0.0 - (x0 + 10.0)).abs() < 1e-9)
                    }
                })
                .unwrap_or_else(|| panic!("no bottom band found at x={x0}: {objects:?}"))
        };
        let left_of = |x0: f64| {
            objects
                .iter()
                .position(|o| {
                    is_new(o) && {
                        let a = &o["path"]["subpaths"][0]["anchors"];
                        let (p0, p1) = (point(&a[0], "p").unwrap(), point(&a[1], "p").unwrap());
                        (p0.0 - p1.0).abs() < 1e-9 && (p0.0 - x0).abs() < 1e-9
                    }
                })
                .unwrap_or_else(|| panic!("no left band found at x={x0}: {objects:?}"))
        };
        // B's left face must sit behind (a lower index than) A's bottom face, not above it; same
        // for C's left face against B's bottom face.
        assert!(left_of(10.0) < bottom_of(0.0), "B's left should be behind A's bottom: {objects:?}");
        assert!(left_of(20.0) < bottom_of(10.0), "C's left should be behind B's bottom: {objects:?}");
    }

    /// The same three letters, but with an eastward-leaning angle: typing order already puts an
    /// earlier letter's depth behind a later one (see the doc comment on the westward test above),
    /// so it must stay untouched — B's bottom face (here the visible pair is North/East, so the
    /// relevant shared faces are the top and right ones instead) must NOT be forced behind A's.
    #[test]
    fn an_eastward_extrusion_keeps_the_original_typing_order() {
        let letter = |id: u64, x: f64| {
            json!({"id": id, "type": "path", "fillRule": "nonzero",
                "path": {"subpaths": [{"closed": true, "anchors": [
                    {"p": [x, 0.0]}, {"p": [x + 10.0, 0.0]}, {"p": [x + 10.0, 10.0]}, {"p": [x, 10.0]}]}]}})
        };
        let out = run_json(&json!({"objects": [letter(1, 0.0), letter(2, 10.0)]}), &json!({"angle": 45, "distance": 15}));
        let objects = out["objects"].as_array().unwrap();
        let is_new = |o: &Value| o.get("id").is_none();
        let top_of = |x0: f64| {
            objects
                .iter()
                .position(|o| {
                    is_new(o) && {
                        let a = &o["path"]["subpaths"][0]["anchors"];
                        let (p0, p1) = (point(&a[0], "p").unwrap(), point(&a[1], "p").unwrap());
                        (p0.1 - p1.1).abs() < 1e-9
                            && ((p0.0 - x0).abs() < 1e-9 && (p1.0 - (x0 + 10.0)).abs() < 1e-9
                                || (p1.0 - x0).abs() < 1e-9 && (p0.0 - (x0 + 10.0)).abs() < 1e-9)
                    }
                })
                .unwrap_or_else(|| panic!("no top band found at x={x0}: {objects:?}"))
        };
        let right_of = |x0: f64| {
            objects
                .iter()
                .position(|o| {
                    is_new(o) && {
                        let a = &o["path"]["subpaths"][0]["anchors"];
                        let (p0, p1) = (point(&a[0], "p").unwrap(), point(&a[1], "p").unwrap());
                        (p0.0 - p1.0).abs() < 1e-9 && (p0.0 - x0).abs() < 1e-9
                    }
                })
                .unwrap_or_else(|| panic!("no right band found at x={x0}: {objects:?}"))
        };
        // A's right face reaches into B's own space here (eastward offset): it must stay behind
        // (a lower index than) B's top face, the natural typing order, unlike the westward case.
        // A spans x in 0..10, so its own right edge sits at x=10 (touching B's left edge).
        assert!(right_of(10.0) < top_of(10.0), "A's right should be behind B's top: {objects:?}");
    }
}
