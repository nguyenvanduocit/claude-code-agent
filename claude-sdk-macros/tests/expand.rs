/// Runtime test: verify that #[claude_tool] generates the expected ToolDef.
///
/// Uses trybuild-free inline expansion by calling the generated _tool_def() fn.
/// We can't directly import claude_sdk::protocol here (the macro generates code
/// that calls it), but we can confirm the generated fn exists and its fields
/// are correct by writing a self-contained struct that mirrors ToolDef.

// Mirror of claude_sdk::protocol::ToolDef so this test has no dependency on the main crate.
#[derive(Debug, PartialEq)]
struct ToolDef {
    name: String,
    description: String,
    input_schema: Option<serde_json::Value>,
}

// Provide the path the macro expands to: claude_sdk::protocol::ToolDef
mod claude_sdk {
    pub mod protocol {
        #[derive(Debug, PartialEq)]
        pub struct ToolDef {
            pub name: String,
            pub description: String,
            pub input_schema: Option<serde_json::Value>,
        }
    }
}

use claude_sdk_macros::claude_tool;

/// Search the filesystem for files matching a pattern
#[claude_tool]
async fn search_files(pattern: String, max_results: u64) -> Vec<String> {
    vec![]
}

/// Toggle a feature flag
#[claude_tool]
async fn set_flag(name: String, enabled: bool, score: f64) -> bool {
    false
}

/// No-param tool
#[claude_tool]
async fn ping() -> String {
    "pong".into()
}

/// Optional param tool
#[claude_tool]
async fn greet(name: String, title: Option<String>) -> String {
    name
}

#[test]
fn search_files_tool_def_name_and_description() {
    let def = search_files_tool_def();
    assert_eq!(def.name, "search_files");
    assert_eq!(def.description, "Search the filesystem for files matching a pattern");
}

#[test]
fn search_files_tool_def_schema() {
    let def = search_files_tool_def();
    let schema = def.input_schema.expect("schema must be Some");

    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["pattern"]["type"], "string");
    assert_eq!(schema["properties"]["max_results"]["type"], "number");

    let required = schema["required"].as_array().expect("required must be array");
    let req_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(req_strs.contains(&"pattern"));
    assert!(req_strs.contains(&"max_results"));
}

#[test]
fn set_flag_schema_bool_and_number() {
    let def = set_flag_tool_def();
    let schema = def.input_schema.unwrap();
    assert_eq!(schema["properties"]["name"]["type"], "string");
    assert_eq!(schema["properties"]["enabled"]["type"], "boolean");
    assert_eq!(schema["properties"]["score"]["type"], "number");
}

#[test]
fn ping_schema_no_params() {
    let def = ping_tool_def();
    assert_eq!(def.name, "ping");
    assert_eq!(def.description, "No-param tool");
    // Should have empty properties and no required
    let schema = def.input_schema.unwrap();
    assert_eq!(schema["type"], "object");
}

#[test]
fn greet_option_not_in_required() {
    let def = greet_tool_def();
    let schema = def.input_schema.unwrap();
    let required = schema["required"].as_array().unwrap();
    let req_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    // name is required, title (Option<String>) is not
    assert!(req_strs.contains(&"name"));
    assert!(!req_strs.contains(&"title"));
    // but title IS in properties
    assert_eq!(schema["properties"]["title"]["type"], "string");
}
