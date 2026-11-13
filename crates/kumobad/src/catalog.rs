use crate::model::{
    CreateRequest, DeleteSecretsRequest, DesktopRequest, ExecRequest, PreviewRequest, RunRequest,
    SecretsRequest, UpdateRequest,
};
use hyper::Method;
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Commands,
    Images,
    AccessLog,
    Create,
    List,
    Get,
    Update,
    Delete,
    Start,
    Stop,
    SetSecrets,
    DeleteSecrets,
    Expose,
    Desktop,
    Exec,
    Run,
    ReadFile,
    WriteFile,
    ListFiles,
}

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    String,
    Integer,
    Strings,
}

pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Clone, Copy)]
pub enum Input {
    None,
    Json(fn() -> Value),
    Bytes,
}

#[derive(Debug, Clone, Copy)]
pub enum Output {
    Json,
    Events,
    Bytes,
}

pub struct Spec {
    pub operation: Operation,
    pub name: &'static str,
    pub summary: &'static str,
    pub method: Method,
    pub path: &'static str,
    pub query: &'static [Param],
    pub input: Input,
    pub output: Output,
    pub rest: Option<&'static str>,
}

pub const BYTES_FIELD: &str = "content";

const fn param(name: &'static str, kind: Kind, description: &'static str) -> Param {
    Param {
        name,
        kind,
        required: false,
        description,
    }
}

const fn required(name: &'static str, kind: Kind, description: &'static str) -> Param {
    Param {
        name,
        kind,
        required: true,
        description,
    }
}

const GUEST_FILE: &[Param] = &[
    required("path", Kind::String, "Absolute path inside the guest"),
    param("user", Kind::String, "Guest user to act as"),
];

fn body<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>()).expect("schema serializes")
}

pub static SPECS: &[Spec] = &[
    Spec {
        operation: Operation::Commands,
        name: "commands",
        summary: "Describe every command this daemon serves",
        method: Method::GET,
        path: "/v1/commands",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Images,
        name: "images",
        summary: "List the images sandboxes can boot",
        method: Method::GET,
        path: "/v1/images",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Run,
        name: "run",
        summary: "Run one command in a fresh sandbox and delete it afterwards",
        method: Method::POST,
        path: "/v1/run",
        query: &[],
        input: Input::Json(body::<RunRequest>),
        output: Output::Events,
        rest: Some("argv"),
    },
    Spec {
        operation: Operation::Create,
        name: "create",
        summary: "Create a sandbox; it starts stopped",
        method: Method::POST,
        path: "/v1/sandboxes",
        query: &[],
        input: Input::Json(body::<CreateRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::List,
        name: "list",
        summary: "List sandboxes, newest first",
        method: Method::GET,
        path: "/v1/sandboxes",
        query: &[
            param(
                "label",
                Kind::Strings,
                "Only sandboxes with this key:value label; repeat to require several",
            ),
            param("limit", Kind::Integer, "Maximum sandboxes to return"),
        ],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Get,
        name: "get",
        summary: "Show one sandbox",
        method: Method::GET,
        path: "/v1/sandboxes/{id}",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Update,
        name: "update",
        summary: "Change a sandbox's labels, timers or network policy",
        method: Method::PATCH,
        path: "/v1/sandboxes/{id}",
        query: &[],
        input: Input::Json(body::<UpdateRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Delete,
        name: "delete",
        summary: "Delete a sandbox and its disk",
        method: Method::DELETE,
        path: "/v1/sandboxes/{id}",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Start,
        name: "start",
        summary: "Boot a stopped sandbox",
        method: Method::POST,
        path: "/v1/sandboxes/{id}/start",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Stop,
        name: "stop",
        summary: "Shut a sandbox down, keeping its disk",
        method: Method::POST,
        path: "/v1/sandboxes/{id}/stop",
        query: &[],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Exec,
        name: "exec",
        summary: "Run a command in a started sandbox and stream its output",
        method: Method::POST,
        path: "/v1/sandboxes/{id}/exec",
        query: &[],
        input: Input::Json(body::<ExecRequest>),
        output: Output::Events,
        rest: Some("argv"),
    },
    Spec {
        operation: Operation::ReadFile,
        name: "read-file",
        summary: "Print a file from the guest",
        method: Method::GET,
        path: "/v1/sandboxes/{id}/files",
        query: GUEST_FILE,
        input: Input::None,
        output: Output::Bytes,
        rest: None,
    },
    Spec {
        operation: Operation::WriteFile,
        name: "write-file",
        summary: "Write a file into the guest",
        method: Method::PUT,
        path: "/v1/sandboxes/{id}/files",
        query: GUEST_FILE,
        input: Input::Bytes,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::ListFiles,
        name: "list-files",
        summary: "List a directory in the guest",
        method: Method::GET,
        path: "/v1/sandboxes/{id}/files/list",
        query: GUEST_FILE,
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Expose,
        name: "expose",
        summary: "Create a signed, expiring link to a port inside the sandbox",
        method: Method::POST,
        path: "/v1/sandboxes/{id}/preview",
        query: &[],
        input: Input::Json(body::<PreviewRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::Desktop,
        name: "desktop",
        summary: "Start the desktop and link to a live view; --open opens it in the browser",
        method: Method::POST,
        path: "/v1/sandboxes/{id}/desktop",
        query: &[],
        input: Input::Json(body::<DesktopRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::SetSecrets,
        name: "set-secrets",
        summary: "Replace a sandbox's secrets",
        method: Method::PUT,
        path: "/v1/sandboxes/{id}/secrets",
        query: &[],
        input: Input::Json(body::<SecretsRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::DeleteSecrets,
        name: "delete-secrets",
        summary: "Remove secrets by name from every sandbox",
        method: Method::POST,
        path: "/v1/secrets/delete",
        query: &[],
        input: Input::Json(body::<DeleteSecretsRequest>),
        output: Output::Json,
        rest: None,
    },
    Spec {
        operation: Operation::AccessLog,
        name: "access-log",
        summary: "Show proxied network connections, allowed and denied",
        method: Method::GET,
        path: "/v1/access-log",
        query: &[
            param(
                "sinceMs",
                Kind::Integer,
                "Start of the window, epoch milliseconds (default two hours ago)",
            ),
            param(
                "untilMs",
                Kind::Integer,
                "End of the window, epoch milliseconds (default now)",
            ),
            param("limit", Kind::Integer, "Maximum entries in this page"),
            param("cursor", Kind::String, "nextCursor from the previous page"),
        ],
        input: Input::None,
        output: Output::Json,
        rest: None,
    },
];

pub struct Resolved {
    pub operation: Operation,
    pub id: Option<String>,
}

fn path_params(template: &str) -> impl Iterator<Item = &str> {
    template
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
}

pub fn resolve(method: &Method, path: &str) -> Option<Resolved> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    SPECS.iter().find_map(|spec| {
        if spec.method != *method {
            return None;
        }
        let template: Vec<&str> = spec.path.trim_matches('/').split('/').collect();
        if template.len() != segments.len() {
            return None;
        }
        let mut id = None;
        for (expected, actual) in template.iter().zip(&segments) {
            if expected.starts_with('{') {
                if actual.is_empty() {
                    return None;
                }
                id = Some((*actual).to_string());
            } else if expected != actual {
                return None;
            }
        }
        Some(Resolved {
            operation: spec.operation,
            id,
        })
    })
}

fn kind_schema(kind: Kind, description: &str) -> Value {
    match kind {
        Kind::String => json!({"type": "string", "description": description}),
        Kind::Integer => json!({"type": "integer", "description": description}),
        Kind::Strings => {
            json!({"type": "array", "items": {"type": "string"}, "description": description})
        }
    }
}

fn describe(spec: &Spec) -> Value {
    let mut properties = Map::new();
    let mut locations = Map::new();
    let mut required = Vec::new();
    let mut positional = Vec::new();
    for name in path_params(spec.path) {
        properties.insert(name.into(), kind_schema(Kind::String, "Sandbox id"));
        locations.insert(name.into(), "path".into());
        required.push(Value::from(name));
        positional.push(Value::from(name));
    }
    for query in spec.query {
        properties.insert(
            query.name.into(),
            kind_schema(query.kind, query.description),
        );
        locations.insert(query.name.into(), "query".into());
        if query.required {
            required.push(query.name.into());
        }
    }
    let input = match spec.input {
        Input::None => "none",
        Input::Json(schema) => {
            let schema = schema();
            if let Some(Value::Object(body)) = schema.get("properties") {
                for (name, property) in body {
                    properties.insert(name.clone(), property.clone());
                    locations.insert(name.clone(), "body".into());
                }
            }
            if let Some(Value::Array(names)) = schema.get("required") {
                required.extend(names.iter().cloned());
            }
            "json"
        }
        Input::Bytes => {
            properties.insert(
                BYTES_FIELD.into(),
                json!({"type": "string", "description": "File content; @path reads a local file, @- reads stdin"}),
            );
            locations.insert(BYTES_FIELD.into(), "bytes".into());
            required.push(BYTES_FIELD.into());
            "bytes"
        }
    };
    let output = match spec.output {
        Output::Json => "json",
        Output::Events => "events",
        Output::Bytes => "bytes",
    };
    json!({
        "name": spec.name,
        "summary": spec.summary,
        "method": spec.method.as_str(),
        "path": spec.path,
        "positional": positional,
        "rest": spec.rest,
        "input": input,
        "output": output,
        "in": locations,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

pub fn catalog() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commands": SPECS.iter().map(describe).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_routes_are_unique() {
        let mut names: Vec<&str> = SPECS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SPECS.len());
        let mut routes: Vec<String> = SPECS
            .iter()
            .map(|spec| format!("{} {}", spec.method, spec.path))
            .collect();
        routes.sort_unstable();
        routes.dedup();
        assert_eq!(routes.len(), SPECS.len());
    }

    #[test]
    fn every_template_resolves_to_its_own_operation() {
        for spec in SPECS {
            let concrete = spec.path.replace("{id}", "sbx-1");
            let resolved = resolve(&spec.method, &concrete).expect(spec.name);
            assert_eq!(resolved.operation, spec.operation, "{}", spec.name);
            assert_eq!(
                resolved.id.as_deref(),
                spec.path.contains("{id}").then_some("sbx-1")
            );
        }
        assert!(resolve(&Method::GET, "/v1/sandboxes/a/b").is_none());
        assert!(resolve(&Method::POST, "/v1/images").is_none());
    }

    #[test]
    fn sdk_fixture_matches_the_catalog() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/typescript/test/fixtures/commands.json");
        let current = json!({"commands": catalog()["commands"]});
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            let mut text = serde_json::to_string_pretty(&current).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let Ok(fixture) = std::fs::read_to_string(&path) else {
            return;
        };
        let fixture: Value = serde_json::from_str(&fixture).unwrap();
        assert_eq!(
            fixture, current,
            "the SDK's catalog fixture is stale; rerun with UPDATE_FIXTURES=1"
        );
    }

    #[test]
    fn catalog_flattens_body_fields_and_marks_locations() {
        let catalog = catalog();
        let commands = catalog["commands"].as_array().unwrap();
        let exec = commands.iter().find(|c| c["name"] == "exec").unwrap();
        assert_eq!(exec["positional"], json!(["id"]));
        assert_eq!(exec["rest"], "argv");
        assert_eq!(exec["output"], "events");
        assert_eq!(exec["in"]["id"], "path");
        assert_eq!(exec["in"]["argv"], "body");
        assert_eq!(
            exec["inputSchema"]["properties"]["timeoutMs"]["type"],
            "integer"
        );
        let required = exec["inputSchema"]["required"].as_array().unwrap();
        assert!(required.contains(&json!("id")) && required.contains(&json!("argv")));
        let create = commands.iter().find(|c| c["name"] == "create").unwrap();
        assert_eq!(
            create["inputSchema"]["properties"]["network"]["properties"]["mode"]["enum"],
            json!(["open", "allowlist", "blocked"])
        );
        let write = commands.iter().find(|c| c["name"] == "write-file").unwrap();
        assert_eq!(write["in"]["content"], "bytes");
        assert_eq!(write["in"]["path"], "query");
        assert!(!catalog.to_string().contains("$ref"));
    }
}
