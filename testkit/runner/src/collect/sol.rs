use serde_json::{Map, Value};

pub fn key_values(line: &str) -> Value {
    let pattern = regex::Regex::new(r#"([A-Za-z][A-Za-z0-9_.-]*)=("[^"]*"|[^\s,()]+)"#).unwrap();
    let mut result = Map::new();
    for capture in pattern.captures_iter(line) {
        let raw = capture[2].trim_matches('"').trim_end_matches(',');
        let numeric = raw.trim_end_matches(['x', '%', 's']);
        result.insert(
            capture[1].to_owned(),
            numeric
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map_or_else(|| Value::String(raw.to_owned()), Value::from),
        );
    }
    Value::Object(result)
}

pub fn parse(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            line.find("SOL op=").map(|index| {
                let raw = line[index..].trim();
                let mut record = key_values(raw);
                record["raw"] = raw.into();
                record
            })
        })
        .collect()
}

pub fn operation(records: &[Value], name: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|row| row["op"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn units_and_quotes() {
        let row = key_values("op=hash actual_s=0.125s sol=3x name=\"two words\" percent=97%");
        assert_eq!(row["actual_s"], 0.125);
        assert_eq!(row["name"], "two words");
        assert_eq!(row["percent"], 97.0);
    }
    #[test]
    fn last_operation_wins() {
        let rows = parse("prefix SOL op=hash sol=1x\nSOL op=hash sol=2x\nother");
        assert_eq!(operation(&rows, "hash")["sol"], 2.0);
        assert!(operation(&rows, "download").is_null());
    }
}
