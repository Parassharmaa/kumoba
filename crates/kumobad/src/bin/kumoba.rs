#[path = "kumoba/args.rs"]
mod args;
#[path = "kumoba/client.rs"]
mod client;

use base64::{Engine, engine::general_purpose::STANDARD};
use client::{Connection, Failure};
use http_body_util::BodyExt;
use serde_json::Value;
use std::io::{IsTerminal, Write};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const LOCAL_HELP: &str = "\
Local commands:
  help [command]       this page, or one command's arguments
  commands [--json]    every command the daemon serves; --json prints the full schemas
  version              this CLI's version and the daemon's

Arguments are positional values, key=value pairs or --kebab-case flags, coerced through
each command's schema. key=@file reads a value from a file, key=@- from stdin, and @@
escapes a literal @. Lists take a,b or repeated flags; maps take repeated key=value.
Everything after -- goes to the command's trailing list, such as the program to run.
--args '{...}' passes the arguments as one JSON object, -q prints only ids, --json prints
compact JSON, -o FILE writes binary output to a file, --open opens a returned url in the
browser (desktop live views and exposed ports).

Connection: KUMOBA_URL (default unix://$KUMOBA_HOME/kumobad.sock), KUMOBA_TOKEN or
KUMOBA_TOKEN_FILE (default $KUMOBA_HOME/token), KUMOBA_HOME (default ~/.kumoba).

Exit status: the guest command's own for exec and run, 1 when kumobad rejects a request,
2 for a usage error, 3 when kumobad cannot be reached.";

fn main() {
    let tokens: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = match runtime.block_on(run(tokens)) {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("kumoba: {}", failure.message());
            failure.exit_code()
        }
    };
    std::process::exit(code);
}

async fn catalog(connection: &Connection) -> Result<Value, Failure> {
    let response = match connection.send("GET", "/v1/commands", None).await {
        Err(Failure::Api(message)) if message.ends_with("(HTTP 404)") => {
            return Err(Failure::Unreachable(format!(
                "kumobad at {} does not describe its commands; upgrade it to {VERSION} or later",
                connection.display
            )));
        }
        other => other?,
    };
    let bytes = client::read_all(response).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| Failure::Api(format!("kumobad sent an unreadable catalog: {error}")))
}

fn find<'a>(catalog: &'a Value, name: &str) -> Option<&'a Value> {
    catalog["commands"]
        .as_array()?
        .iter()
        .find(|command| command["name"] == name)
}

async fn run(tokens: Vec<String>) -> Result<i32, Failure> {
    let first = tokens.first().map(String::as_str);
    if matches!(first, Some("version" | "--version" | "-V")) {
        println!("kumoba {VERSION}");
        if let Ok(connection) = Connection::from_env()
            && let Ok(catalog) = catalog(&connection).await
        {
            println!(
                "kumobad {}",
                catalog["version"].as_str().unwrap_or("unknown")
            );
        }
        return Ok(0);
    }
    let wants_help = matches!(first, None | Some("help" | "--help" | "-h"));
    let connection = match Connection::from_env() {
        Ok(connection) => connection,
        Err(failure) if wants_help && tokens.len() <= 1 => {
            print_overview(None, &failure);
            return Ok(0);
        }
        Err(failure) => return Err(failure),
    };
    let catalog = match catalog(&connection).await {
        Ok(catalog) => catalog,
        Err(failure) if wants_help && tokens.len() <= 1 => {
            print_overview(None, &failure);
            return Ok(0);
        }
        Err(failure) => return Err(failure),
    };
    match first {
        None | Some("--help" | "-h") => {
            print_overview(
                Some((&catalog, &connection)),
                &Failure::Usage(String::new()),
            );
            Ok(0)
        }
        Some("help") => match tokens.get(1) {
            Some(name) => {
                let command = find(&catalog, name)
                    .ok_or_else(|| Failure::Usage(format!("unknown command {name}")))?;
                print_command(command);
                Ok(0)
            }
            None => {
                print_overview(
                    Some((&catalog, &connection)),
                    &Failure::Usage(String::new()),
                );
                Ok(0)
            }
        },
        Some("commands") if find(&catalog, "commands").is_some() => {
            if tokens.iter().any(|token| token == "--json") {
                println!("{}", serde_json::to_string_pretty(&catalog).expect("json"));
            } else {
                for command in catalog["commands"].as_array().into_iter().flatten() {
                    println!(
                        "{:<16} {}",
                        command["name"].as_str().unwrap_or(""),
                        command["summary"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Some(name) => {
            let spec = find(&catalog, name).ok_or_else(|| {
                Failure::Usage(format!("unknown command {name}; `kumoba help` lists them"))
            })?;
            invoke(&connection, spec, &tokens[1..]).await
        }
    }
}

fn print_overview(found: Option<(&Value, &Connection)>, failure: &Failure) {
    println!("kumoba {VERSION}: command line for a kumobad sandbox daemon\n");
    println!("Usage: kumoba <command> [arguments]   kumoba <command> --help\n");
    match found {
        Some((catalog, connection)) => {
            println!(
                "Commands served by kumobad {} at {}:",
                catalog["version"].as_str().unwrap_or("unknown"),
                connection.display
            );
            for command in catalog["commands"].as_array().into_iter().flatten() {
                let name = command["name"].as_str().unwrap_or("");
                if name == "commands" {
                    continue;
                }
                println!(
                    "  {:<16} {}",
                    name,
                    command["summary"].as_str().unwrap_or("")
                );
            }
        }
        None => println!(
            "The command list comes from the daemon, which is not reachable now:\n  {}\n",
            failure.message()
        ),
    }
    println!("\n{LOCAL_HELP}");
}

fn type_label(schema: &Value) -> String {
    if let Some(Value::Array(values)) = schema.get("enum") {
        return values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|");
    }
    let kinds = args::types(schema);
    let kind = kinds
        .iter()
        .find(|kind| **kind != "null")
        .unwrap_or(&"string");
    match *kind {
        "array" => format!("{}[]", type_label(&schema["items"])),
        "object" if schema.get("properties").is_none() => "key=value".into(),
        other => other.into(),
    }
}

fn rows(
    schema: &Value,
    prefix: &str,
    required: &[&str],
    rest: Option<&str>,
    out: &mut Vec<(String, String, String)>,
) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (name, property) in properties {
        let dotted = format!("{prefix}{}", args::kebab(name));
        if property.get("properties").is_some() {
            rows(property, &format!("{dotted}."), &[], None, out);
            continue;
        }
        let mut note = property["description"].as_str().unwrap_or("").to_string();
        match property.get("default") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) if map.is_empty() => {}
            Some(Value::Array(list)) if list.is_empty() => {}
            Some(default) => note = format!("{note} (default {default})"),
        }
        if rest == Some(name.as_str()) {
            note = format!("{note}; or everything after --");
        }
        let flag = if required.contains(&name.as_str()) {
            "required"
        } else {
            "optional"
        };
        out.push((
            format!("--{dotted} <{}>", type_label(property)),
            flag.into(),
            note,
        ));
    }
}

fn print_command(spec: &Value) {
    let name = spec["name"].as_str().unwrap_or("");
    println!(
        "kumoba {name}: {}\n",
        spec["summary"].as_str().unwrap_or("")
    );
    let mut usage = format!("kumoba {name}");
    for positional in spec["positional"].as_array().into_iter().flatten() {
        usage.push_str(&format!(" <{}>", positional.as_str().unwrap_or("")));
    }
    usage.push_str(" [--flag value | key=value ...]");
    if let Some(rest) = spec["rest"].as_str() {
        usage.push_str(&format!(" -- <{}...>", args::kebab(rest)));
    }
    println!("Usage\n  {usage}\n");
    let required: Vec<&str> = spec["inputSchema"]["required"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut table = Vec::new();
    rows(
        &spec["inputSchema"],
        "",
        &required,
        spec["rest"].as_str(),
        &mut table,
    );
    if !table.is_empty() {
        let width = table.iter().map(|row| row.0.len()).max().unwrap_or(0);
        println!("Arguments");
        for (flag, status, note) in table {
            println!("  {flag:<width$}  {status:<8}  {note}");
        }
    }
    let output = match spec["output"].as_str() {
        Some("events") => "the command's stdout and stderr, and its exit status",
        Some("bytes") => "the raw bytes, on stdout or into -o FILE",
        _ => "JSON",
    };
    println!(
        "\nPrints {output}. {} {}",
        spec["method"].as_str().unwrap_or(""),
        spec["path"].as_str().unwrap_or("")
    );
}

fn query_value(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().flat_map(query_value).collect(),
        Value::String(text) => vec![text.clone()],
        Value::Null => vec![],
        other => vec![other.to_string()],
    }
}

async fn invoke(connection: &Connection, spec: &Value, tokens: &[String]) -> Result<i32, Failure> {
    let command = args::Command::new(spec);
    let mut parsed = command.parse(tokens).map_err(Failure::Usage)?;
    if parsed.help {
        print_command(spec);
        return Ok(0);
    }
    if command.bytes_field.is_some() && parsed.bytes.is_none() && !std::io::stdin().is_terminal() {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
            .map_err(|error| Failure::Usage(format!("cannot read stdin: {error}")))?;
        parsed.bytes = Some(bytes);
    }
    let missing = command.missing(&parsed);
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|name| args::kebab(name)).collect();
        return Err(Failure::Usage(format!(
            "missing {}; `kumoba {} --help` shows the arguments",
            names.join(", "),
            spec["name"].as_str().unwrap_or("")
        )));
    }
    let mut path = spec["path"].as_str().unwrap_or("").to_string();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    let mut body = serde_json::Map::new();
    for (name, value) in &parsed.values {
        match spec["in"][name].as_str() {
            Some("path") => {
                let text = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string);
                let encoded: String =
                    url::form_urlencoded::byte_serialize(text.as_bytes()).collect();
                path = path.replace(&format!("{{{name}}}"), &encoded);
            }
            Some("query") => {
                for item in query_value(value) {
                    query.append_pair(name, &item);
                }
            }
            _ => {
                body.insert(name.clone(), value.clone());
            }
        }
    }
    let query = query.finish();
    if !query.is_empty() {
        path = format!("{path}?{query}");
    }
    let payload = match spec["input"].as_str() {
        Some("bytes") => Some((
            "application/octet-stream",
            parsed.bytes.take().unwrap_or_default(),
        )),
        Some("json") => Some((
            "application/json",
            serde_json::to_vec(&body).expect("json body"),
        )),
        _ => None,
    };
    let method = spec["method"].as_str().unwrap_or("GET");
    let response = connection.send(method, &path, payload).await?;
    match spec["output"].as_str() {
        Some("events") => stream_events(response).await,
        Some("bytes") => {
            let bytes = client::read_all(response).await?;
            match &parsed.output {
                Some(file) => std::fs::write(file, &bytes)
                    .map_err(|error| Failure::Usage(format!("cannot write {file}: {error}")))?,
                None => std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|error| Failure::Api(error.to_string()))?,
            }
            Ok(0)
        }
        _ => {
            let bytes = client::read_all(response).await?;
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            print_json(&value, &parsed);
            if parsed.open {
                open_url(&value)?;
            }
            Ok(0)
        }
    }
}

fn open_url(value: &Value) -> Result<(), Failure> {
    let Some(url) = value.get("url").and_then(Value::as_str) else {
        return Err(Failure::Api(
            "kumobad returned no browser URL; start it with KUMOBAD_LISTEN or KUMOBAD_PUBLIC_URL set"
                .into(),
        ));
    };
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .status()
        .map_err(|error| Failure::Api(format!("cannot run {opener}: {error}")))?;
    Ok(())
}

fn ids(value: &Value) -> Vec<String> {
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        return vec![id.to_string()];
    }
    value
        .as_object()
        .into_iter()
        .flat_map(|object| object.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn print_json(value: &Value, parsed: &args::Parsed) {
    if parsed.quiet {
        for id in ids(value) {
            println!("{id}");
        }
    } else if parsed.json || !std::io::stdout().is_terminal() {
        println!("{value}");
    } else {
        println!("{}", serde_json::to_string_pretty(value).expect("json"));
    }
}

async fn stream_events(response: hyper::Response<hyper::body::Incoming>) -> Result<i32, Failure> {
    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| Failure::Api(format!("output stream broke: {error}")))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buffer.extend_from_slice(&data);
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let Ok(event) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            match event["type"].as_str() {
                Some(kind @ ("stdout" | "stderr")) => {
                    let data = STANDARD
                        .decode(event["data"].as_str().unwrap_or(""))
                        .unwrap_or_default();
                    let target: &mut dyn Write = if kind == "stdout" {
                        &mut stdout
                    } else {
                        &mut stderr
                    };
                    let _ = target.write_all(&data);
                    let _ = target.flush();
                }
                Some("exit") => {
                    if event["timedOut"] == true {
                        eprintln!("kumoba: command timed out");
                        return Ok(124);
                    }
                    let code = event["exitCode"].as_i64().unwrap_or(1);
                    return Ok(i32::try_from(code).unwrap_or(1));
                }
                Some("error") => {
                    return Err(Failure::Api(
                        event["message"]
                            .as_str()
                            .unwrap_or("command failed")
                            .to_string(),
                    ));
                }
                _ => {}
            }
        }
    }
    Err(Failure::Api(
        "output stream ended before the command exited".into(),
    ))
}
