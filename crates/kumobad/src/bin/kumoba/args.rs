use serde_json::{Map, Value};
use std::io::Read;

#[derive(Default)]
pub struct Parsed {
    pub values: Map<String, Value>,
    pub bytes: Option<Vec<u8>>,
    pub help: bool,
    pub quiet: bool,
    pub json: bool,
    pub open: bool,
    pub output: Option<String>,
}

pub fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for character in name.chars() {
        if character == '-' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

pub fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for character in name.chars() {
        if character.is_uppercase() {
            out.push('-');
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

pub fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ if schema.get("enum").is_some() => vec!["string"],
        _ => vec![],
    }
}

fn primary(schema: &Value) -> &str {
    types(schema)
        .into_iter()
        .find(|kind| *kind != "null")
        .unwrap_or("string")
}

fn property<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut schema = root;
    for name in path {
        schema = schema.get("properties")?.get(name)?;
    }
    Some(schema)
}

fn read_source(value: &str) -> Result<Vec<u8>, String> {
    if value == "@-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        return Ok(bytes);
    }
    if let Some(literal) = value.strip_prefix("@@") {
        return Ok(format!("@{literal}").into_bytes());
    }
    if let Some(path) = value.strip_prefix('@') {
        return std::fs::read(path).map_err(|error| format!("cannot read {path}: {error}"));
    }
    Ok(value.as_bytes().to_vec())
}

fn text(value: &str) -> Result<String, String> {
    String::from_utf8(read_source(value)?).map_err(|_| "argument file is not UTF-8".to_string())
}

fn scalar(schema: &Value, raw: &str, name: &str) -> Result<Value, String> {
    if raw == "null" && types(schema).contains(&"null") {
        return Ok(Value::Null);
    }
    let invalid = |kind: &str| format!("{name} must be {kind}, got {raw}");
    match primary(schema) {
        "integer" => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| invalid("an integer")),
        "number" => raw
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| invalid("a number")),
        "boolean" => match raw {
            "true" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "0" | "no" => Ok(Value::Bool(false)),
            _ => Err(invalid("true or false")),
        },
        _ => {
            if let Some(Value::Array(allowed)) = schema.get("enum")
                && !allowed.iter().any(|value| value == raw)
            {
                let names: Vec<String> = allowed.iter().map(ToString::to_string).collect();
                return Err(format!("{name} must be one of {}", names.join(", ")));
            }
            Ok(Value::String(raw.to_string()))
        }
    }
}

fn coerce(schema: &Value, raw: &str, name: &str, current: Option<&Value>) -> Result<Value, String> {
    if primary(schema) == "object"
        && !raw.starts_with('@')
        && !raw.trim_start().starts_with('{')
        && let Some((key, value)) = raw.split_once('=')
    {
        let mut map = match current {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        map.insert(key.to_string(), Value::String(text(value)?));
        return Ok(Value::Object(map));
    }
    let raw = text(raw)?;
    match primary(schema) {
        "array" => {
            if raw.trim_start().starts_with('[') {
                return serde_json::from_str(&raw).map_err(|error| format!("{name}: {error}"));
            }
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            let mut values = match current {
                Some(Value::Array(existing)) => existing.clone(),
                _ => Vec::new(),
            };
            for part in raw
                .split([',', '\n'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                values.push(scalar(&items, part, name)?);
            }
            Ok(Value::Array(values))
        }
        "object" => {
            if raw.trim_start().starts_with('{') {
                return serde_json::from_str(&raw).map_err(|error| format!("{name}: {error}"));
            }
            let Some((key, value)) = raw.split_once('=') else {
                return Err(format!("{name} takes JSON or key=value pairs, got {raw}"));
            };
            let mut map = match current {
                Some(Value::Object(existing)) => existing.clone(),
                _ => Map::new(),
            };
            map.insert(key.to_string(), Value::String(value.to_string()));
            Ok(Value::Object(map))
        }
        _ => scalar(schema, &raw, name),
    }
}

fn insert(values: &mut Map<String, Value>, path: &[String], value: Value) {
    let (last, parents) = path.split_last().expect("non-empty path");
    let mut target = values;
    for name in parents {
        let entry = target
            .entry(name.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        target = entry.as_object_mut().expect("object");
    }
    target.insert(last.clone(), value);
}

fn get<'a>(values: &'a Map<String, Value>, path: &[String]) -> Option<&'a Value> {
    let (last, parents) = path.split_last()?;
    let mut target = values;
    for name in parents {
        target = target.get(name)?.as_object()?;
    }
    target.get(last)
}

pub struct Command<'a> {
    pub spec: &'a Value,
    pub schema: &'a Value,
    pub bytes_field: Option<&'a str>,
}

impl<'a> Command<'a> {
    pub fn new(spec: &'a Value) -> Self {
        let bytes_field = spec["in"]
            .as_object()
            .and_then(|locations| locations.iter().find(|(_, place)| *place == "bytes"))
            .map(|(name, _)| name.as_str());
        Self {
            spec,
            schema: &spec["inputSchema"],
            bytes_field,
        }
    }

    fn path_of(&self, key: &str) -> Option<Vec<String>> {
        let path: Vec<String> = key.split('.').map(camel).collect();
        property(self.schema, &path).map(|_| path)
    }

    fn set(&self, parsed: &mut Parsed, path: &[String], raw: &str) -> Result<(), String> {
        let name = path.join(".");
        if self.bytes_field == Some(name.as_str()) {
            parsed.bytes = Some(read_source(raw)?);
            return Ok(());
        }
        let schema = property(self.schema, path).expect("known property");
        let value = coerce(schema, raw, &kebab(&name), get(&parsed.values, path))?;
        insert(&mut parsed.values, path, value);
        Ok(())
    }

    pub fn parse(&self, tokens: &[String]) -> Result<Parsed, String> {
        let mut parsed = Parsed::default();
        let (before, rest) = match tokens.iter().position(|token| token == "--") {
            Some(index) => (&tokens[..index], Some(&tokens[index + 1..])),
            None => (tokens, None),
        };
        let positional: Vec<&str> = self.spec["positional"]
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut next_positional = 0;
        let mut index = 0;
        while index < before.len() {
            let token = &before[index];
            index += 1;
            match token.as_str() {
                "--help" | "-h" => {
                    parsed.help = true;
                    continue;
                }
                "--quiet" | "-q" => {
                    parsed.quiet = true;
                    continue;
                }
                "--json" => {
                    parsed.json = true;
                    continue;
                }
                "--open" => {
                    parsed.open = true;
                    continue;
                }
                "--output" | "-o" => {
                    parsed.output = Some(
                        before
                            .get(index)
                            .ok_or("--output needs a file path")?
                            .clone(),
                    );
                    index += 1;
                    continue;
                }
                "--args" => {
                    let raw = text(before.get(index).ok_or("--args needs a JSON object")?)?;
                    index += 1;
                    let Value::Object(object) =
                        serde_json::from_str(&raw).map_err(|error| format!("--args: {error}"))?
                    else {
                        return Err("--args must be a JSON object".into());
                    };
                    parsed.values.extend(object);
                    continue;
                }
                _ => {}
            }
            if let Some(flag) = token.strip_prefix("--") {
                let (key, inline) = match flag.split_once('=') {
                    Some((key, value)) => (key, Some(value.to_string())),
                    None => (flag, None),
                };
                let path = self
                    .path_of(key)
                    .ok_or_else(|| format!("unknown option --{key}"))?;
                let schema = property(self.schema, &path).expect("known property");
                let raw = match inline {
                    Some(value) => value,
                    None if primary(schema) == "boolean"
                        && before.get(index).is_none_or(|next| next.starts_with("--")) =>
                    {
                        "true".to_string()
                    }
                    None => {
                        let value = before
                            .get(index)
                            .ok_or_else(|| format!("--{key} needs a value"))?;
                        index += 1;
                        value.clone()
                    }
                };
                self.set(&mut parsed, &path, &raw)?;
                continue;
            }
            if let Some((key, value)) = token.split_once('=')
                && let Some(path) = self.path_of(key)
            {
                self.set(&mut parsed, &path, value)?;
                continue;
            }
            let name = positional
                .get(next_positional)
                .ok_or_else(|| format!("unexpected argument {token}"))?;
            next_positional += 1;
            self.set(&mut parsed, &[(*name).to_string()], token)?;
        }
        if let Some(rest) = rest {
            let name = self.spec["rest"]
                .as_str()
                .ok_or("this command takes no arguments after --")?;
            parsed.values.insert(
                name.to_string(),
                Value::Array(rest.iter().cloned().map(Value::String).collect()),
            );
        }
        Ok(parsed)
    }

    pub fn missing(&self, parsed: &Parsed) -> Vec<String> {
        self.schema["required"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| {
                        if self.bytes_field == Some(*name) {
                            parsed.bytes.is_none()
                        } else {
                            !parsed.values.contains_key(*name)
                        }
                    })
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Value {
        json!({
            "name": "exec",
            "positional": ["id"],
            "rest": "argv",
            "in": {"id": "path", "argv": "body", "env": "body", "timeoutMs": "body", "network": "body", "tty": "body", "label": "query", "content": "bytes", "autoDeleteMinutes": "body"},
            "inputSchema": {
                "type": "object",
                "required": ["id", "argv"],
                "properties": {
                    "id": {"type": "string"},
                    "argv": {"type": "array", "items": {"type": "string"}},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}},
                    "timeoutMs": {"type": "integer"},
                    "tty": {"type": "boolean"},
                    "label": {"type": "array", "items": {"type": "string"}},
                    "content": {"type": "string"},
                    "autoDeleteMinutes": {"type": ["integer", "null"]},
                    "network": {"type": "object", "properties": {
                        "mode": {"type": "string", "enum": ["open", "allowlist", "blocked"]},
                        "hosts": {"type": "array", "items": {"type": "string"}}
                    }}
                }
            }
        })
    }

    fn parse(tokens: &[&str]) -> Result<Parsed, String> {
        let spec = spec();
        let command = Command::new(&spec);
        command.parse(&tokens.iter().map(ToString::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn positional_flags_pairs_and_rest() {
        let parsed = parse(&[
            "sbx-1",
            "--timeout-ms",
            "5000",
            "env=A=1",
            "--env",
            "B=2",
            "--tty",
            "--",
            "python3",
            "-c",
            "print(1)",
        ])
        .unwrap();
        assert_eq!(
            Value::Object(parsed.values),
            json!({"id": "sbx-1", "timeoutMs": 5000, "env": {"A": "1", "B": "2"}, "tty": true, "argv": ["python3", "-c", "print(1)"]})
        );
    }

    #[test]
    fn dotted_keys_lists_enums_and_null() {
        let parsed = parse(&[
            "--network.mode",
            "allowlist",
            "network.hosts=a.com,b.com",
            "--label",
            "x:1",
            "--label=y:2",
            "--auto-delete-minutes",
            "null",
        ])
        .unwrap();
        assert_eq!(
            Value::Object(parsed.values),
            json!({"network": {"mode": "allowlist", "hosts": ["a.com", "b.com"]}, "label": ["x:1", "y:2"], "autoDeleteMinutes": null})
        );
        assert!(parse(&["--network.mode", "sometimes"]).is_err());
        assert!(parse(&["--timeout-ms", "soon"]).is_err());
        assert!(parse(&["--nope", "1"]).is_err());
    }

    #[test]
    fn files_literals_args_and_required() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("payload.txt");
        std::fs::write(&file, "from a file").unwrap();
        let parsed = parse(&[
            &format!("content=@{}", file.display()),
            "--args",
            r#"{"id":"sbx-2"}"#,
            "env=K=@@literal",
        ])
        .unwrap();
        assert_eq!(parsed.bytes.as_deref(), Some(&b"from a file"[..]));
        assert_eq!(parsed.values["id"], "sbx-2");
        assert_eq!(parsed.values["env"], json!({"K": "@literal"}));
        let spec = spec();
        let command = Command::new(&spec);
        assert_eq!(command.missing(&parsed), ["argv"]);
    }

    #[test]
    fn names_convert_between_cases() {
        assert_eq!(camel("memory-mib"), "memoryMib");
        assert_eq!(kebab("autoStopMinutes"), "auto-stop-minutes");
    }
}
