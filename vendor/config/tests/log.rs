extern crate config;
extern crate log;

#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use config::*;

#[derive(Debug, Deserialize)]
struct Settings {
    log: log::Level,
}

fn config(s: &str) -> Config {
    Config::builder()
        .add_source(File::from_str(s, FileFormat::Json))
        .build()
        .unwrap()
}

#[test]
fn test_load_level_uppercase() {
    let s = r#"{ "log": "ERROR" }"#;
    let c = config(s);
    let l = c.get::<log::Level>("log").unwrap();
    assert_eq!(l, log::Level::Error);
}

#[test]
fn test_case_sensitivity_log_level_from_str() {
    // to verify that this works

    use std::str::FromStr;
    let l = log::Level::from_str("error").unwrap();
    assert_eq!(l, log::Level::Error);
}

#[test]
fn test_case_sensitivity_json_from_str() {
    // to confirm serde_json works as expected
    let s = r#"{ "log": "error" }"#;

    let j: Settings = serde_json::from_str(s).unwrap();
    assert_eq!(j.log, log::Level::Error);
}

#[test]
fn test_load_level_lowercase_succeeding() {
    let s = r#"{ "log": "error" }"#;
    let c = config(s);
    assert_eq!(c.get_string("log").unwrap(), "error");
    let s = c.try_deserialize::<Settings>();
    assert!(s.is_ok(), "Expected Ok(_) for {:?}", s);
    assert_eq!(s.unwrap().log, log::Level::Error);
}
