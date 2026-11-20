/// Compile-checks that #[claude_tool] output integrates with claude_sdk::protocol::ToolDef.
///
/// This is an integration test run with: cargo test --features macros
/// It confirms the generated <name>_tool_def() -> ToolDef can be passed
/// directly into Initialize::tools without any type conversion.
#[cfg(feature = "macros")]
mod macro_integration {
    use claude_sdk::protocol::{Initialize, ToolDef};
    use claude_sdk_macros::claude_tool;

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
        // Generated fn returns the real claude_sdk::protocol::ToolDef.
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

    #[test]
    fn tool_def_can_go_into_initialize_tools() {
        // The key integration check: ToolDef from macro slots into Initialize::tools.
        let init = Initialize {
            kind: "initialize".into(),
            cwd: "/tmp".into(),
            tools: vec![list_files_tool_def(), convert_temp_tool_def()],
            ..Default::default()
        };
        assert_eq!(init.tools.len(), 2);
        assert_eq!(init.tools[0].name, "list_files");
        assert_eq!(init.tools[1].name, "convert_temp");
    }
}
