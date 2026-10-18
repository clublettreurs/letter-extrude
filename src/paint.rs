//! The paint the extruded faces are filled with.

use serde_json::{Value, json};

/// The object's top fill, else its top stroke's paint, else a neutral grey: what Extrude & Bevel
/// paints its faces with too.
pub(crate) fn top_paint(obj: &Value) -> Value {
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
pub(crate) fn darkened(v: &Value, amt: f64) -> Value {
    let mut v = v.clone();
    darken(&mut v, amt);
    v
}

pub(crate) fn darken(v: &mut Value, amt: f64) {
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
