//! Tools the local model can call: built-in app connectors, MCP servers, delegation, mail.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Map, Value, json};

/// Arguments the model passed to a tool, with lenient accessors (models send "5" for 5).
#[derive(Debug, Clone, Default)]
pub struct Args(pub Map<String, Value>);

impl Args {
    pub fn from_value(v: Value) -> Self {
        match v {
            Value::Object(m) => Args(m),
            Value::String(s) => serde_json::from_str::<Map<String, Value>>(&s).map(Args).unwrap_or_default(),
            _ => Args::default(),
        }
    }

    pub fn str(&self, key: &str) -> String {
        match self.0.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        }
    }

    pub fn int(&self, key: &str, default: i64) -> i64 {
        match self.0.get(key) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
            _ => default,
        }
    }

    pub fn bool(&self, key: &str) -> bool {
        match self.0.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => matches!(s.to_lowercase().as_str(), "true" | "yes" | "1"),
            Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
            _ => false,
        }
    }

    /// A list of strings given either as an array or as a comma-separated string.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Value::Array(a)) => {
                a.iter().filter_map(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
            }
            Some(Value::String(s)) => s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            _ => vec![],
        }
    }

    pub fn require(&self, key: &str) -> anyhow::Result<String> {
        let v = self.str(key);
        if v.is_empty() {
            anyhow::bail!("missing argument {key:?}");
        }
        Ok(v)
    }
}

pub type Handler = Arc<dyn Fn(Args) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// Wrap an async closure as a tool handler.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Args) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
{
    Arc::new(move |args| Box::pin(f(args)))
}

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub handler: Handler,
    /// Shown in the UI while the tool runs, e.g. "Searching notes".
    pub status: String,
}

impl Tool {
    pub fn new(name: &str, description: &str, parameters: Value, handler: Handler, status: &str) -> Self {
        Self { name: name.into(), description: description.into(), parameters, handler, status: status.into() }
    }

    pub fn spec(&self) -> Value {
        json!({"type": "function", "function": {
            "name": self.name, "description": self.description, "parameters": self.parameters}})
    }
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).finish()
    }
}

/// JSON schema for an object with these properties.
pub fn params(required: &[&str], props: &[(&str, Value)]) -> Value {
    let properties: Map<String, Value> = props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    json!({"type": "object", "properties": properties, "required": required})
}

/// A schema property: `{"type": ..., "description": ...}`.
pub fn prop(type_: &str, desc: &str) -> Value {
    json!({"type": type_, "description": desc})
}

/// Name-ordered set of tools. Cheap to clone (handlers are shared).
#[derive(Clone, Default, Debug)]
pub struct ToolRegistry {
    pub tools: BTreeMap<String, Tool>,
    order: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, tool: Tool) {
        if !self.tools.contains_key(&tool.name) {
            self.order.push(tool.name.clone());
        }
        self.tools.insert(tool.name.clone(), tool);
    }

    pub fn remove(&mut self, name: &str) {
        self.tools.remove(name);
        self.order.retain(|n| n != name);
    }

    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools.get(name)
    }

    #[allow(dead_code)]
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Tools in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &Tool> {
        self.order.iter().filter_map(|n| self.tools.get(n))
    }

    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }

    pub fn without(&self, names: &[String]) -> ToolRegistry {
        let mut reg = ToolRegistry::new();
        for t in self.iter().filter(|t| !names.contains(&t.name)) {
            reg.add(t.clone());
        }
        reg
    }

    #[allow(dead_code)]
    pub fn specs(&self) -> Vec<Value> {
        self.iter().map(Tool::spec).collect()
    }

    /// Run a tool; the result is always a string for the model (errors included).
    pub async fn call(&self, name: &str, args: Args) -> String {
        let Some(tool) = self.tools.get(name) else {
            return format!("error: unknown tool {name:?}");
        };
        match (tool.handler)(args).await {
            Ok(Value::String(s)) => s,
            Ok(v) => serde_json::to_string(&v).unwrap_or_default(),
            Err(e) => {
                tracing::warn!("tool {name} failed: {e:#}");
                format!("error: {e:#}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_calls_and_errors() {
        let mut reg = ToolRegistry::new();
        reg.add(Tool::new(
            "echo",
            "Echo",
            params(&["text"], &[("text", prop("string", ""))]),
            handler(|a: Args| async move { Ok(json!(a.require("text")?)) }),
            "Echoing",
        ));
        reg.add(Tool::new("obj", "Obj", params(&[], &[]), handler(|_| async { Ok(json!({"a": 1})) }), ""));
        assert_eq!(reg.call("echo", Args::from_value(json!({"text": "hi"}))).await, "hi");
        assert_eq!(reg.call("obj", Args::default()).await, r#"{"a":1}"#);
        assert!(reg.call("echo", Args::default()).await.starts_with("error: missing argument"));
        assert!(reg.call("nope", Args::default()).await.starts_with("error: unknown tool"));
        assert_eq!(reg.names(), vec!["echo", "obj"]);
        assert_eq!(reg.without(&["echo".into()]).names(), vec!["obj"]);
    }

    #[test]
    fn lenient_args() {
        let a = Args::from_value(json!({"n": "5", "f": 2.0, "b": "true", "to": "a@x, b@y", "l": ["c@z"]}));
        assert_eq!((a.int("n", 0), a.int("f", 0), a.int("missing", 7)), (5, 2, 7));
        assert!(a.bool("b") && !a.bool("missing"));
        assert_eq!(a.list("to"), vec!["a@x", "b@y"]);
        assert_eq!(a.list("l"), vec!["c@z"]);
        let s = Args::from_value(json!(r#"{"x": 1}"#));
        assert_eq!(s.int("x", 0), 1);
    }
}
