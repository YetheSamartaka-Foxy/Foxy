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
        "missing".to_owned()
    } else if let Some(s) = value.as_str() {
        s.to_owned()
    } else {
        value.to_string()
    }
}
/// A numeric gate needs a finite number to judge. A missing, null, `na` or
/// non-numeric value is a failure of its own rather than an implicit zero
/// that would slip through a maximum or a zero minimum.
fn gated_number(actual: &Value) -> Result<f64, String> {
    match actual.as_f64() {
        Some(number) if number.is_finite() => Ok(number),
        _ => Err(format!("expected a number, got {}", display(actual))),
    }
}
pub fn expectations(view: &Value, expects: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for expected in expects.as_array().into_iter().flatten() {
        let path = expected["path"].as_str().unwrap_or("");
        let actual = dotted(view, path);
        // `optional: true` declares a metric that may legitimately be absent
        // (a lane the operation did not run); its gates apply only when it is.
        if actual.is_null() && expected["optional"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(equal) = expected.get("equals")
            && actual != equal
        {
            failures.push(format!(
                "{path} expected {}, got {}",
                display(equal),
                display(actual)
            ));
        }
        let numeric_gate = expected["min"].is_number()
            || expected["max"].is_number()
            || expected["between"].is_array();
        if !numeric_gate {
            continue;
        }
        let number = match gated_number(actual) {
            Ok(number) => number,
            Err(reason) => {
                failures.push(format!("{path} {reason}"));
                continue;
            }
        };
        for (key, word, bad) in [("min", "at least", true), ("max", "at most", false)] {
            if let Some(limit) = expected[key].as_f64()
                && (if bad { number < limit } else { number > limit })
            {
                failures.push(format!("{path} expected {word} {limit}, got {number}"));
            }
        }
        if let Some(band) = expected["between"].as_array()
            && band.len() == 2
            && (number < band[0].as_f64().unwrap_or(0.0)
                || number > band[1].as_f64().unwrap_or(0.0))
        {
            failures.push(format!(
                "{path} expected between {} and {}, got {number}",
                display(&band[0]),
                display(&band[1]),
            ));
        }
    }
    failures
}
pub fn thresholds(view: &Value, thresholds: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, gate) in thresholds.as_object().into_iter().flatten() {
        let actual = dotted(view, path);
        if actual.is_null() && gate["optional"].as_bool().unwrap_or(false) {
            continue;
        }
        let number = match gated_number(actual) {
            Ok(number) => number,
            Err(reason) => {
                failures.push(format!("{path} {reason}"));
                continue;
            }
        };
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
    #[test]
    fn missing_or_non_numeric_values_fail_numeric_gates() {
        let v = json!({"sol":"na","zero":0,"nan":null,"text":"x"});
        // A maximum gate on a missing value used to pass as zero.
        assert_eq!(
            expectations(&v, &json!([{"path":"absent","max":10}])),
            ["absent expected a number, got missing"]
        );
        assert_eq!(
            expectations(&v, &json!([{"path":"absent","min":0}])).len(),
            1
        );
        assert_eq!(
            expectations(&v, &json!([{"path":"sol","min":0.5}])),
            ["sol expected a number, got na"]
        );
        assert_eq!(
            expectations(&v, &json!([{"path":"nan","between":[0,1]}])).len(),
            1
        );
        // An actual zero remains a valid number.
        assert!(
            expectations(
                &v,
                &json!([{"path":"zero","max":0},{"path":"zero","min":0}])
            )
            .is_empty()
        );
        // `equals` on text needs no number.
        assert!(expectations(&v, &json!([{"path":"text","equals":"x"}])).is_empty());
        assert_eq!(thresholds(&v, &json!({"absent":{"max":1}})).len(), 1);
        assert_eq!(thresholds(&v, &json!({"sol":{"min":0.5}})).len(), 1);
        assert!(thresholds(&v, &json!({"zero":{"max":0}})).is_empty());
    }
    #[test]
    fn optional_metrics_may_be_absent_but_not_wrong() {
        let v = json!({"present":5});
        assert!(expectations(&v, &json!([{"path":"absent","max":1,"optional":true}])).is_empty());
        assert_eq!(
            expectations(&v, &json!([{"path":"present","max":1,"optional":true}])).len(),
            1
        );
        assert!(thresholds(&v, &json!({"absent":{"max":1,"optional":true}})).is_empty());
    }
}
