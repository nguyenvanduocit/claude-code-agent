/// Compile-checks that #[claude_tool] output integrates with claude_code_agent::protocol::ToolDef.
///
/// This is an integration test run with: cargo test --features macros
/// It confirms the generated <name>_tool_def() returns claude_code_agent::protocol::ToolDef.
#[cfg(feature = "macros")]
mod macro_integration {
    #![allow(unused_variables, dead_code)]
    use claude_code_agent::protocol::ToolDef;
    use claude_code_agent_macros::claude_tool;

    /// List files in a directory
    #[claude_tool]
    async fn list_files(path: String, recursive: bool) -> Vec<String> {
        vec![]
    }

    /// Convert temperature between units
    #[claude_tool]
    async fn convert_temp(value: f64, from_unit: String, to_unit: Option<String>) -> f64 {
        0.0
    }

    #[test]
    fn tool_def_is_correct_type() {
        // Generated fn returns the real claude_code_agent::protocol::ToolDef.
        let def: ToolDef = list_files_tool_def();
        assert_eq!(def.name, "list_files");
        assert_eq!(def.description, "List files in a directory");
        let schema = def.input_schema.unwrap();
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["recursive"]["type"], "boolean");
        let req: Vec<&str> = schema["required"]
            .as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"path"));
        assert!(req.contains(&"recursive"));
    }

    #[test]
    fn option_param_excluded_from_required() {
        let def: ToolDef = convert_temp_tool_def();
        let schema = def.input_schema.unwrap();
        let req: Vec<&str> = schema["required"]
            .as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"value"));
        assert!(req.contains(&"from_unit"));
        assert!(!req.contains(&"to_unit")); // Option<String> → not required
        // but to_unit IS in properties
        assert_eq!(schema["properties"]["to_unit"]["type"], "string");
    }

    /// Divide two numbers
    #[claude_tool]
    fn divide(a: f64, b: f64) -> Result<f64, String> {
        if b == 0.0 { Err("division by zero".into()) } else { Ok(a / b) }
    }

    /// Greet someone
    #[claude_tool]
    async fn greet(name: &str) -> String {
        format!("hello {name}")
    }

    async fn call(server: &claude_code_agent::SdkMcpServer, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });
        server.handle(&msg).await["result"].clone()
    }

    #[tokio::test]
    async fn generated_handler_decodes_args_and_maps_results() {
        let server = claude_code_agent::SdkMcpServer::new("t").tool(divide_tool()).tool(greet_tool());

        let ok = call(&server, "divide", serde_json::json!({ "a": 9, "b": 3 })).await;
        assert_eq!(ok["content"][0]["text"], "3.0");
        assert!(ok.get("isError").is_none());

        let err = call(&server, "divide", serde_json::json!({ "a": 1, "b": 0 })).await;
        assert_eq!(err["content"][0]["text"], "division by zero");
        assert_eq!(err["isError"], true);

        let bad = call(&server, "divide", serde_json::json!({ "a": "x" })).await;
        assert_eq!(bad["isError"], true);

        let hi = call(&server, "greet", serde_json::json!({ "name": "rust" })).await;
        assert_eq!(hi["content"][0]["text"], "hello rust");
    }

    type CheckResult<T> = Result<T, String>;

    /// Fails through a `Result` alias
    #[claude_tool]
    async fn always_fails() -> CheckResult<u32> {
        Err("disk on fire".into())
    }

    /// Show a path
    #[claude_tool]
    fn show_path(path: std::path::PathBuf, base: &std::path::Path, tags: &[String]) -> String {
        format!("{} {} {}", base.join(path).display(), tags.len(), tags.join(","))
    }

    /// Echo any JSON
    #[claude_tool]
    fn echo(value: serde_json::Value) -> serde_json::Value {
        value
    }

    #[tokio::test]
    async fn alias_result_err_is_an_error_result() {
        let server = claude_code_agent::SdkMcpServer::new("t").tool(always_fails_tool());
        let err = call(&server, "always_fails", serde_json::json!({})).await;
        assert_eq!(err["content"][0]["text"], "disk on fire");
        assert_eq!(err["isError"], true);
    }

    #[tokio::test]
    async fn path_and_any_params_have_matching_schema_and_decode() {
        let schema = show_path_tool_def().input_schema.unwrap();
        assert_eq!(schema["properties"]["path"], serde_json::json!({ "type": "string" }));
        assert_eq!(schema["properties"]["base"], serde_json::json!({ "type": "string" }));
        assert_eq!(schema["properties"]["tags"], serde_json::json!({ "type": "array", "items": { "type": "string" } }));
        let schema = echo_tool_def().input_schema.unwrap();
        assert_eq!(schema["properties"]["value"], serde_json::json!({}));

        let server = claude_code_agent::SdkMcpServer::new("t").tool(show_path_tool()).tool(echo_tool());
        let shown = call(&server, "show_path", serde_json::json!({ "path": "b.txt", "base": "/a", "tags": ["x", "y"] })).await;
        let joined = std::path::Path::new("/a").join("b.txt");
        assert_eq!(shown["content"][0]["text"], format!("{} 2 x,y", joined.display()));
        assert!(shown.get("isError").is_none());
        let echoed = call(&server, "echo", serde_json::json!({ "value": { "k": [1, 2] } })).await;
        assert_eq!(echoed["content"][0]["text"], r#"{"k":[1,2]}"#);
    }
}
