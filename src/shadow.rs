//! The cast shadow: every letter's back face extruded again, merged into one shape.

use serde_json::{Value, json};

use crate::faces::{Look, segment_bands};
use crate::geom::*;
use crate::output::*;

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
pub(crate) fn shadow_object(objects: &[&Value], main_offset: P, shadow_offset: P, side_amt: f64) -> Option<OutItem> {
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
pub(crate) fn subpaths_with_role(obj: &Value) -> Vec<(Value, bool)> {
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

/// The same as [`bands_for_subpath`], but reading a subpath already built as an [`OutSubpath`]
/// (e.g. a back face) instead of input JSON: used to extrude the back face again for the shadow,
/// which stays a single uniform tone and is never pruned (always passes `face: None`).
pub(crate) fn bands_for_out_subpath(sp: &OutSubpath, offset: P) -> Vec<OutSubpath> {
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
                sign,
                &Look::uniform(),
            ))
        })
        .flatten()
        .map(|(sp, _)| sp)
        .collect()
}
