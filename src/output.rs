//! The output objects (serialized straight to JSON) and the subpath operations on them.

use serde::Serialize;
use serde_json::Value;

use crate::geom::*;

/// One anchor of a generated back face or band: a plain struct, not a `Value`, so serializing
/// thousands of them (one letterform's worth of bands) writes their field names straight from
/// `&'static str` instead of allocating a `String` key per field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct OutAnchor {
    pub(crate) p: [f64; 2],
    #[serde(rename = "in", skip_serializing_if = "Option::is_none")]
    pub(crate) h_in: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) out: Option<[f64; 2]>,
}

pub(crate) fn out_anchor(p: P, h_in: Option<P>, h_out: Option<P>) -> OutAnchor {
    OutAnchor { p: [p.0, p.1], h_in: h_in.map(|h| [h.0, h.1]), out: h_out.map(|h| [h.0, h.1]) }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct OutSubpath {
    pub(crate) anchors: Vec<OutAnchor>,
    pub(crate) closed: bool,
}

#[derive(Serialize)]
pub(crate) struct OutPathData {
    pub(crate) subpaths: Vec<OutSubpath>,
}

/// A new path object: the back face or one side band. `fills` still carries the paint as a
/// `Value` (arbitrary solid/gradient/pattern JSON), but there is only one per object, not one per
/// anchor, so its cost doesn't scale with the number of bands.
#[derive(Serialize)]
pub(crate) struct NewObj {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) path: OutPathData,
    #[serde(rename = "fillRule")]
    pub(crate) fill_rule: &'static str,
    pub(crate) fills: [Value; 1],
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum OutItem {
    New(NewObj),
    Kept { id: Value },
}

#[derive(Serialize)]
pub(crate) struct Output {
    pub(crate) objects: Vec<OutItem>,
}

/// The shoelace signed area of `sp`'s anchor points (ignoring curvature, which doesn't change the
/// sign): which way it winds.
pub(crate) fn signed_area(sp: &OutSubpath) -> f64 {
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
pub(crate) fn wound_like(sp: OutSubpath, reference: f64) -> OutSubpath {
    if signed_area(&sp) * reference < 0.0 { reverse_out_subpath(sp) } else { sp }
}

/// The same closed shape traced the other way: anchors in reverse order, each with its own
/// in/out handles swapped (as the native path model's own `reverse` does).
pub(crate) fn reverse_out_subpath(mut sp: OutSubpath) -> OutSubpath {
    sp.anchors.reverse();
    for a in &mut sp.anchors {
        std::mem::swap(&mut a.h_in, &mut a.out);
    }
    sp
}

/// The closed, 4-anchor band between edge `a`(`a_out`) → `b`(`b_in`) and its copy offset by
/// `offset`: the original curve, a straight connector, the same curve reversed (offset), and a
/// straight connector back.
pub(crate) fn band(a: P, a_out: Option<P>, b: P, b_in: Option<P>, offset: P) -> OutSubpath {
    let (a2, b2) = (add(a, offset), add(b, offset));
    let anchors = vec![
        out_anchor(a, None, a_out),
        out_anchor(b, b_in, None),
        out_anchor(b2, None, b_in.map(|h| add(h, offset))),
        out_anchor(a2, a_out.map(|h| add(h, offset)), None),
    ];
    OutSubpath { anchors, closed: true }
}

pub(crate) fn shift_subpath(sp: &Value, offset: P) -> OutSubpath {
    let closed = sp.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let anchors: Vec<OutAnchor> = sp.get("anchors").and_then(Value::as_array).into_iter().flatten().map(|a| shift_anchor(a, offset)).collect();
    OutSubpath { anchors, closed }
}

/// [`shift_subpath`], but shifting a subpath already built as an [`OutSubpath`] (e.g. a back face)
/// instead of input JSON: used to extrude the back face again for the shadow.
pub(crate) fn shift_out_subpath(sp: &OutSubpath, offset: P) -> OutSubpath {
    let shift = |p: [f64; 2]| [p[0] + offset.0, p[1] + offset.1];
    let anchors = sp.anchors.iter().map(|a| OutAnchor { p: shift(a.p), h_in: a.h_in.map(shift), out: a.out.map(shift) }).collect();
    OutSubpath { anchors, closed: sp.closed }
}

pub(crate) fn shift_anchor(a: &Value, offset: P) -> OutAnchor {
    let p = point(a, "p").unwrap_or((0.0, 0.0));
    out_anchor(add(p, offset), point(a, "in").map(|h| add(h, offset)), point(a, "out").map(|h| add(h, offset)))
}

/// The average position of a band's anchors, projected on the unit offset direction `dir` (for
/// back-to-front sorting).
pub(crate) fn projection(sp: &OutSubpath, dir: P) -> f64 {
    if sp.anchors.is_empty() {
        return 0.0;
    }
    let (sx, sy) = sp.anchors.iter().fold((0.0, 0.0), |(sx, sy), a| (sx + a.p[0], sy + a.p[1]));
    let n = sp.anchors.len() as f64;
    (sx / n) * dir.0 + (sy / n) * dir.1
}
