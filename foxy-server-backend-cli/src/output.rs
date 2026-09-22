use anyhow::Result;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static JSON_MODE: AtomicBool = AtomicBool::new(false);
static DETAILS: OnceLock<Mutex<Option<Value>>> = OnceLock::new();

pub fn set_json_mode(enabled: bool) {
    JSON_MODE.store(enabled, Ordering::Relaxed);
}

pub fn json_mode() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

pub fn set_details(value: Value) {
    *DETAILS.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(value);
}

pub fn insert_detail(key: &str, value: Value) {
    let mut details = DETAILS.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some(Value::Object(object)) = details.as_mut() {
        object.insert(key.to_string(), value);
    }
}

pub fn replace_output_path(output: &std::path::Path) {
    insert_detail("output", json!(output));
}

pub fn print_result(command: &str, result: &Result<()>) {
    let details = DETAILS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .take();
    let value = match result {
        Ok(()) => json!({"status": "ok", "command": command, "details": details}),
        Err(err) => {
            json!({"status": "error", "command": command, "error": format!("{err:#}"), "details": details})
        }
    };
    std::println!("{}", value);
}

pub fn print_parse_error(message: &str) {
    std::println!(
        "{}",
        json!({"status": "error", "command": null, "error": message, "details": null})
    );
}
