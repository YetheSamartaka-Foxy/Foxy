use serde_json::Value;

pub fn dotted<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(value, |current, key| {
        if current.is_array() {
            key.parse::<usize>()
                .ok()
                .and_then(|i| current.get(i))
                .unwrap_or(&Value::Null)
        } else {
            current.get(key).unwrap_or(&Value::Null)
        }
    })
}
fn display(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else if let Some(s) = value.as_str() {
        s.to_owned()
    } else {
        value.to_string()
    }
}
pub fn expectations(view: &Value, expects: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for expected in expects.as_array().into_iter().flatten() {
        let path = expected["path"].as_str().unwrap_or("");
        let actual = dotted(view, path);
        let number = actual.as_f64().unwrap_or(0.0);
        if let Some(equal) = expected.get("equals")
            && actual != equal
        {
            failures.push(format!(
                "{path} expected {}, got {}",
                display(equal),
                display(actual)
            ));
        }
        for (key, word, bad) in [("min", "at least", true), ("max", "at most", false)] {
            if let Some(limit) = expected[key].as_f64()
                && (if bad { number < limit } else { number > limit })
            {
                failures.push(format!(
                    "{path} expected {word} {limit}, got {}",
                    display(actual)
                ));
            }
        }
        if let Some(band) = expected["between"].as_array()
            && band.len() == 2
            && (number < band[0].as_f64().unwrap_or(0.0)
                || number > band[1].as_f64().unwrap_or(0.0))
        {
            failures.push(format!(
                "{path} expected between {} and {}, got {}",
                display(&band[0]),
                display(&band[1]),
                display(actual)
            ));
        }
    }
    failures
}
pub fn thresholds(view: &Value, thresholds: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, gate) in thresholds.as_object().into_iter().flatten() {
        let number = dotted(view, path).as_f64().unwrap_or(0.0);
        if gate["min"].as_f64().is_some_and(|min| number < min) {
            failures.push(format!("{path} below minimum"));
        }
        if gate["max"].as_f64().is_some_and(|max| number > max) {
            failures.push(format!("{path} above maximum"));
        }
    }
    failures
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn arrays_and_missing() {
        let v = json!({"items":[{"n":3}]});
        assert_eq!(dotted(&v, "items.0.n"), 3);
        assert!(dotted(&v, "items.1.n").is_null());
        assert_eq!(
            expectations(&v, &json!([{"path":"items.0.n","min":4}])).len(),
            1
        );
        assert!(thresholds(&v, &json!({"items.0.n":{"max":3}})).is_empty());
    }
}
