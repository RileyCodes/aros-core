//! Tool registry — Claude tool use framework
//!
//! Tools are registered at engine startup. When Claude returns tool_calls,
//! the registry dispatches to the right handler.

use serde_json::Value;
use std::collections::HashMap;

/// Result of executing a tool
#[derive(Debug)]
pub struct ToolResult {
    pub success: bool,
    pub message: String,
}

/// Tool definition for Claude API
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Tool handler function type
pub type ToolHandler = Box<dyn Fn(Value) -> ToolResult + Send + Sync>;

/// Registry of available tools
pub struct ToolRegistry {
    defs: Vec<ToolDef>,
    handlers: HashMap<String, ToolHandler>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            defs: Vec::new(),
            handlers: HashMap::new(),
        }
    }

    /// Register a tool with its definition and handler
    pub fn register<F>(&mut self, name: &str, description: &str, parameters: Value, handler: F)
    where
        F: Fn(Value) -> ToolResult + Send + Sync + 'static,
    {
        self.defs.push(ToolDef {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        });
        self.handlers.insert(name.to_string(), Box::new(handler));
    }

    /// Generate the tools array for Claude API request
    pub fn to_api_json(&self) -> Value {
        Value::Array(
            self.defs
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": d.name,
                            "description": d.description,
                            "parameters": d.parameters
                        }
                    })
                })
                .collect(),
        )
    }

    /// Execute a tool call by name
    pub fn execute(&self, name: &str, args: Value) -> Option<ToolResult> {
        self.handlers.get(name).map(|handler| {
            log::info!("Tool execute: {} args={}", name, args);
            handler(args)
        })
    }

    /// Check if a tool exists
    pub fn has(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// Number of registered tools
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_execute() {
        let mut reg = ToolRegistry::new();
        reg.register(
            "test_tool",
            "A test tool",
            serde_json::json!({"type": "object", "properties": {}}),
            |_args| ToolResult {
                success: true,
                message: "done".to_string(),
            },
        );

        assert_eq!(reg.len(), 1);
        assert!(reg.has("test_tool"));
        assert!(!reg.has("nonexistent"));

        let result = reg.execute("test_tool", serde_json::json!({}));
        assert!(result.is_some());
        assert!(result.unwrap().success);
    }

    #[test]
    fn test_api_json_format() {
        let mut reg = ToolRegistry::new();
        reg.register(
            "update_memory",
            "Update user memory",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string"}
                }
            }),
            |_| ToolResult {
                success: true,
                message: "ok".to_string(),
            },
        );

        let json = reg.to_api_json();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["function"]["name"], "update_memory");
    }

    #[test]
    fn test_tool_with_args() {
        let mut reg = ToolRegistry::new();
        reg.register(
            "greet",
            "Greet someone",
            serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            |args| {
                let name = args["name"].as_str().unwrap_or("world");
                ToolResult {
                    success: true,
                    message: format!("Hello, {}!", name),
                }
            },
        );

        let result = reg
            .execute("greet", serde_json::json!({"name": "Riley"}))
            .unwrap();
        assert_eq!(result.message, "Hello, Riley!");
    }

    #[test]
    fn test_multiple_tools() {
        let mut reg = ToolRegistry::new();
        reg.register("tool_a", "desc a", serde_json::json!({}), |_| ToolResult {
            success: true,
            message: "a".into(),
        });
        reg.register("tool_b", "desc b", serde_json::json!({}), |_| ToolResult {
            success: false,
            message: "b failed".into(),
        });

        assert_eq!(reg.len(), 2);
        assert!(reg.has("tool_a"));
        assert!(reg.has("tool_b"));

        let json = reg.to_api_json();
        assert_eq!(json.as_array().unwrap().len(), 2);

        let r = reg.execute("tool_b", serde_json::json!({})).unwrap();
        assert!(!r.success);
        assert_eq!(r.message, "b failed");
    }

    #[test]
    fn test_empty_registry() {
        let reg = ToolRegistry::new();
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
        assert_eq!(reg.to_api_json(), serde_json::json!([]));
    }

    #[test]
    fn test_execute_nonexistent() {
        let reg = ToolRegistry::new();
        assert!(reg.execute("nope", serde_json::json!({})).is_none());
    }
}
