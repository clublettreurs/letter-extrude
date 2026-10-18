use super::*;
use crate::paint::*;
use serde_json::json;

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
                let kept = face_tone(tangent, sign, &Look { face: Some(face), ..Look::new(angle, None) }).is_some();
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
    let bands = bands_for_subpath(&json!({"closed": true, "anchors": [{"p": [0.0, 0.0]}, {"p": [10.0, 0.0]}]}), (5.0, -5.0), 1.0, &Look::uniform());
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
    let bands = bands_for_subpath(&sp, (0.0, -30.0), 1.0, &Look::uniform());
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
    let bands = bands_for_subpath(&sp, offset, 1.0, &Look::uniform());
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
    assert_eq!(bands_for_subpath(&sp, (0.0, -30.0), 1.0, &Look::uniform()).len(), 1);
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
    let out = run_json(&json!({"objects": [obj]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}));
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
    let cap1_sp = subpaths.iter().find(|sp| matches_cap(sp, &cap1)).unwrap_or_else(|| panic!("the counter's own back face not found: {subpaths:?}"));
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
    let out = run_json(&json!({"objects": [obj]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 135, "shadowDistance": 5}));
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
    let out =
        run_json(&json!({"objects": [letter(0.0)]}), &json!({"angle": 45, "distance": 15, "shadow": true, "shadowAngle": 225, "shadowDistance": 10}));
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
    let out = run_json(&json!({"objects": [open]}), &json!({"angle": 90, "distance": 10, "shadow": true, "shadowAngle": 180, "shadowDistance": 5}));
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
    // visible coverage, then each circle splits once more where its tone changes (8 kept), plus
    // the one merged back face and the kept front.
    assert_eq!(out["objects"].as_array().unwrap().len(), 10, "{out}");
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
        let mut anchors: Vec<Value> = pts.iter().map(|&(px, py, ix, iy, ox, oy)| json!({"p": [px, py], "in": [ix, iy], "out": [ox, oy]})).collect();
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
    assert_eq!(out["objects"].as_array().unwrap().len(), 40 * 10, "{out}");
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
    let bands = segment_bands(p0, Some(p1), p3, Some(p2), compass_offset(angle, 10.0), 1.0, &Look::new(angle, None));
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
    let tone = face_tone(tangent, 1.0, &Look { dark_reference: 90.0, ..Look::new(angle, None) });
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

/// With a shadow, a curved face changes tone where the shadow's outline touches it (outward
/// bearing `shadow ± 90°`) when that point lies between the two visible faces; else halfway.
#[test]
fn the_tone_boundary_follows_the_shadow() {
    assert_eq!(tone_split(180.0, 270.0, None), 225.0);
    assert_eq!(tone_split(180.0, 270.0, Some(115.0)), 205.0);
    assert_eq!(tone_split(180.0, 270.0, Some(20.0)), 225.0, "290° and 110° both lie outside South..West");
}

/// A "T" extruded towards the South-East with its shadow at 160°: the bar casts a shade line on
/// the stem's lit East face, from the inner corner (55, 20) to the stem's back edge. Hand-worked:
/// offset (28.28, 28.28), shadow direction (0.342, 0.940), in the band's own coordinates the
/// shadow is 0.00747 along the edge and 0.01209 in depth, so it reaches the back edge (x = 83.28)
/// at y = 20 + 0.940 / 0.01209 = 97.7.
#[test]
fn a_concave_corner_casts_a_shade_line_on_the_lit_face_next_to_it() {
    let t = |shadow: bool| {
        let anchors: Vec<Value> = [(0.0, 0.0), (90.0, 0.0), (90.0, 20.0), (55.0, 20.0), (55.0, 100.0), (35.0, 100.0), (35.0, 20.0), (0.0, 20.0)]
            .iter()
            .map(|(x, y)| json!({"p": [x, y]}))
            .collect();
        let doc = json!({"objects": [{"id": 1, "type": "path", "fillRule": "nonzero",
            "fills": [{"type": "solid", "color": {"model": "rgb", "r": 1.0, "g": 0.0, "b": 0.0}}],
            "path": {"subpaths": [{"closed": true, "anchors": anchors}]}}]});
        let params = json!({"angle": 135, "distance": 40, "sideShade": 35, "sideContrast": 30, "shadow": shadow, "shadowAngle": 160});
        serde_json::from_slice::<Value>(&run(&doc, &params)).unwrap()
    };
    let triangles = |out: &Value| -> Vec<Value> {
        out["objects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["path"]["subpaths"][0]["anchors"].as_array().is_some_and(|a| a.len() == 3))
            .cloned()
            .collect()
    };
    assert!(triangles(&t(false)).is_empty(), "no shade line without a shadow");
    let out = t(true);
    let tri = triangles(&out);
    assert_eq!(tri.len(), 1, "{out}");
    let pts: Vec<(f64, f64)> = tri[0]["path"]["subpaths"][0]["anchors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["p"][0].as_f64().unwrap(), a["p"][1].as_f64().unwrap()))
        .collect();
    let near = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).abs() < 0.05 && (p.1 - q.1).abs() < 0.05;
    let o = 40.0 * std::f64::consts::FRAC_1_SQRT_2;
    for expected in [(55.0, 20.0), (55.0 + o, 97.7), (55.0 + o, 20.0 + o)] {
        assert!(pts.iter().any(|&p| near(p, expected)), "{expected:?} missing from {pts:?}");
    }
    assert!((tri[0]["fills"][0]["color"]["r"].as_f64().unwrap() - 0.5).abs() < 1e-9, "the shaded part takes the dark tone");
}
