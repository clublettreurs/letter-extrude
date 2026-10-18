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

mod faces;
mod geom;
mod output;
mod paint;
mod shadow;
#[cfg(test)]
mod tests;

use serde_json::Value;

use faces::*;
use geom::*;
use output::*;
use shadow::*;

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
    // away from the light), else South stays dark (an overhead light, as in ordinary lettering);
    // with a shadow, concave corners also cast a shade line on the faces next to them. See
    // [`face_tone`] and [`Look`].
    let look = Look::new(angle, shadow_on.then_some(shadow_angle));
    let mut per_letter: Vec<(Option<OutItem>, Vec<(f64, OutItem)>, Option<OutItem>)> =
        valid.iter().map(|obj| extrude_one(obj, offset, side_amt, contrast, &look)).collect();
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

fn pack(bytes: &[u8]) -> u64 {
    ((bytes.len() as u64) << 32) | bytes.as_ptr() as u64
}
