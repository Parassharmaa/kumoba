use crate::macos::Vm;
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{AGENT_PORT, ExecEvent, ExecRequest, MAX_EXEC_FRAME, OutputKind, Request};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

const SCRIPT: &str = r##"uid=$(/usr/bin/stat -f %u /dev/console)
if [ "$uid" -eq 0 ]; then echo "nobody is signed in to the sandbox's desktop" >&2; exit 3; fi
desk() { /bin/launchctl asuser "$uid" /usr/bin/sudo -u "#$uid" /usr/bin/env LANG=en_US.UTF-8 "$@"; }
count() { desk /usr/bin/osascript -l JavaScript -e 'ObjC.import("AppKit"); $.NSPasteboard.generalPasteboard.changeCount'; }
case "$1" in
count) count ;;
copied)
  i=0
  while [ "$i" -lt 12 ]; do
    if [ "$(count)" != "$KUMOBA_VALUE" ]; then desk /usr/bin/pbpaste; exit 0; fi
    i=$((i + 1))
    /bin/sleep 0.05
  done
  exit 4 ;;
read) desk /usr/bin/pbpaste ;;
set) desk /usr/bin/pbcopy ;;
swap) desk /usr/bin/pbpaste && desk /usr/bin/pbcopy ;;
restore) if [ "$(desk /usr/bin/pbpaste)" = "$KUMOBA_VALUE" ]; then desk /usr/bin/pbcopy; fi ;;
esac"##;
const WAIT: Duration = Duration::from_secs(10);
const LIMIT: usize = 1 << 20;
const ERRORS: usize = 4 * 1024;
const NOTHING_NEW: i32 = 4;

#[derive(Clone, Copy)]
pub enum Action {
    Count,
    Copied,
    Read,
    Set,
    Swap,
    Restore,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Copied => "copied",
            Self::Read => "read",
            Self::Set => "set",
            Self::Swap => "swap",
            Self::Restore => "restore",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Output {
    pub text: Vec<u8>,
    pub whole: bool,
}

fn write<T: Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(body.len()).map_err(io::Error::other)?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&body)
}

fn read<T: DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let length = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
    if length > MAX_EXEC_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the guest sent an oversized frame",
        ));
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(io::Error::other)
}

pub fn run(
    mut stream: UnixStream,
    action: Action,
    input: &[u8],
    value: &str,
) -> Result<Option<Output>, String> {
    let failed = |error: io::Error| format!("the sandbox's clipboard did not answer: {error}");
    stream.set_read_timeout(Some(WAIT)).map_err(failed)?;
    stream.set_write_timeout(Some(WAIT)).map_err(failed)?;
    let mut env = BTreeMap::new();
    env.insert("KUMOBA_VALUE".to_string(), value.to_string());
    let request = Request::Exec(ExecRequest {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            SCRIPT.into(),
            "kumoba-clipboard".into(),
            action.name().into(),
        ],
        cwd: "/".into(),
        env,
        stdin: STANDARD.encode(input),
        timeout_ms: WAIT.as_millis().try_into().unwrap_or(u64::MAX),
        output_limit: LIMIT,
        user: None,
    });
    write(&mut stream, &request).map_err(failed)?;
    let (mut text, mut errors, mut dropped) = (Vec::new(), Vec::new(), false);
    loop {
        let event: ExecEvent = read(&mut stream).map_err(failed)?;
        stream.write_all(&[1]).map_err(failed)?;
        match event {
            ExecEvent::Output { stream: kind, data } => {
                let data = STANDARD.decode(data).map_err(|error| error.to_string())?;
                let (target, cap) = match kind {
                    OutputKind::Stdout => (&mut text, LIMIT),
                    OutputKind::Stderr => (&mut errors, ERRORS),
                };
                let room = cap.saturating_sub(target.len());
                dropped |= matches!(kind, OutputKind::Stdout) && data.len() > room;
                target.extend_from_slice(&data[..data.len().min(room)]);
            }
            ExecEvent::Finished {
                code: Some(0),
                truncated,
                ..
            } => {
                return Ok(Some(Output {
                    text,
                    whole: !truncated && !dropped,
                }));
            }
            ExecEvent::Finished {
                code: Some(NOTHING_NEW),
                ..
            } => return Ok(None),
            ExecEvent::Finished { .. } => {
                let reason = String::from_utf8_lossy(&errors).trim().to_string();
                return Err(if reason.is_empty() {
                    "the sandbox's clipboard could not be reached".into()
                } else {
                    reason
                });
            }
            ExecEvent::Failed { message } => return Err(message),
        }
    }
}

pub fn clipboard(
    vm: &Vm,
    action: Action,
    input: &[u8],
    value: &str,
) -> Result<Option<Output>, String> {
    run(
        UnixStream::from(vm.connect(AGENT_PORT)?),
        action,
        input,
        value,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(
        events: Vec<ExecEvent>,
    ) -> (UnixStream, std::thread::JoinHandle<(ExecRequest, usize)>) {
        let (host, mut guest) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let Request::Exec(request) = read::<Request>(&mut guest).unwrap() else {
                panic!("expected an exec request");
            };
            let mut acknowledged = 0;
            for event in events {
                write(&mut guest, &event).unwrap();
                let mut byte = [0u8; 1];
                guest.read_exact(&mut byte).unwrap();
                acknowledged += usize::from(byte[0] == 1);
            }
            (request, acknowledged)
        });
        (host, fake)
    }

    fn output(stream: OutputKind, text: &str) -> ExecEvent {
        ExecEvent::Output {
            stream,
            data: STANDARD.encode(text),
        }
    }

    fn finished(code: i32, truncated: bool) -> ExecEvent {
        ExecEvent::Finished {
            code: Some(code),
            signal: None,
            timed_out: false,
            truncated,
        }
    }

    #[test]
    fn text_goes_in_through_stdin_and_every_answer_is_acknowledged() {
        let (host, fake) = agent(vec![
            output(OutputKind::Stdout, "ünï "),
            output(OutputKind::Stdout, "日本"),
            finished(0, false),
        ]);
        let answer = run(host, Action::Swap, "it's \"quoted\" $HOME".as_bytes(), "").unwrap();
        let (request, acknowledged) = fake.join().unwrap();
        assert_eq!(
            answer,
            Some(Output {
                text: "ünï 日本".as_bytes().to_vec(),
                whole: true
            })
        );
        assert_eq!(acknowledged, 3);
        assert_eq!(
            STANDARD.decode(&request.stdin).unwrap(),
            "it's \"quoted\" $HOME".as_bytes()
        );
        assert_eq!(request.args.last().map(String::as_str), Some("swap"));
        assert!(
            request
                .args
                .iter()
                .all(|argument| !argument.contains("quoted"))
        );
    }

    #[test]
    fn a_clipboard_cut_short_by_the_guest_is_marked_so() {
        let (host, fake) = agent(vec![output(OutputKind::Stdout, "part"), finished(0, true)]);
        let answer = run(host, Action::Swap, b"", "").unwrap().unwrap();
        fake.join().unwrap();
        assert!(!answer.whole);
    }

    #[test]
    fn a_guest_that_sends_more_than_allowed_is_cut_off_on_the_host() {
        let chunk = "x".repeat(MAX_EXEC_FRAME / 2);
        let mut events: Vec<ExecEvent> = (0..LIMIT / chunk.len() + 2)
            .map(|_| output(OutputKind::Stdout, &chunk))
            .collect();
        events.extend((0..4).map(|_| output(OutputKind::Stderr, &chunk)));
        events.push(finished(0, false));
        let (host, fake) = agent(events);
        let answer = run(host, Action::Swap, b"", "").unwrap().unwrap();
        fake.join().unwrap();
        assert_eq!(answer.text.len(), LIMIT);
        assert!(!answer.whole);
    }

    #[test]
    fn nothing_new_on_the_clipboard_is_not_an_error() {
        let (host, fake) = agent(vec![finished(NOTHING_NEW, false)]);
        assert_eq!(run(host, Action::Copied, b"", "7"), Ok(None));
        let (request, _) = fake.join().unwrap();
        assert_eq!(
            request.env.get("KUMOBA_VALUE").map(String::as_str),
            Some("7")
        );
    }

    #[test]
    fn a_read_returns_whatever_is_on_the_clipboard_now() {
        let (host, fake) = agent(vec![
            output(OutputKind::Stdout, "copied from a menu"),
            finished(0, false),
        ]);
        let answer = run(host, Action::Read, b"", "").unwrap();
        let (request, _) = fake.join().unwrap();
        assert_eq!(
            answer.map(|answer| answer.text),
            Some(b"copied from a menu".to_vec())
        );
        assert_eq!(request.args.last().map(String::as_str), Some("read"));
    }

    #[test]
    fn the_guest_reason_is_passed_on_when_it_fails() {
        let (host, fake) = agent(vec![
            output(
                OutputKind::Stderr,
                "nobody is signed in to the sandbox's desktop\n",
            ),
            finished(3, false),
        ]);
        assert_eq!(
            run(host, Action::Set, b"x", ""),
            Err("nobody is signed in to the sandbox's desktop".into())
        );
        fake.join().unwrap();
    }
}
