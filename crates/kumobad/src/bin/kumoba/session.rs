use crate::{
    client::{Connection, Failure},
    terminal::{Frame, PROTOCOL, read_frame, write_frame},
};
use std::io::{IsTerminal, Write};
use tokio::{io::AsyncReadExt, sync::mpsc};

struct RawMode(Option<libc::termios>);

impl RawMode {
    fn enable() -> Self {
        if !std::io::stdin().is_terminal() {
            return Self(None);
        }
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &raw mut original) } != 0 {
            return Self(None);
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&raw mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const raw) } != 0 {
            return Self(None);
        }
        Self(Some(original))
    }

    fn restore(&mut self) {
        if let Some(original) = self.0.take() {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const original) };
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        self.restore();
    }
}

pub fn window_size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    for fd in [1, 0, 2] {
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut size) } == 0 && size.ws_row > 0 {
            return Some((size.ws_row, size.ws_col));
        }
    }
    None
}

pub async fn run(connection: &Connection, path: &str, body: Vec<u8>) -> Result<i32, Failure> {
    let upgraded = connection.upgrade(path, body, PROTOCOL).await?;
    let (mut reader, mut writer) = tokio::io::split(hyper_util::rt::TokioIo::new(upgraded));
    let mut raw = RawMode::enable();
    let (inputs, mut pending) = mpsc::channel::<Frame>(64);
    let stdin_inputs = inputs.clone();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buffer = vec![0u8; 8192];
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) | Err(_) => {
                    let _ = stdin_inputs.send(Frame::Eof).await;
                    break;
                }
                Ok(read) => {
                    if stdin_inputs
                        .send(Frame::Data(buffer[..read].to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    if let Ok(mut resized) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
    {
        let resize_inputs = inputs.clone();
        tokio::spawn(async move {
            while resized.recv().await.is_some() {
                if let Some((rows, cols)) = window_size()
                    && resize_inputs
                        .send(Frame::Resize { rows, cols })
                        .await
                        .is_err()
                {
                    break;
                }
            }
        });
    }
    drop(inputs);
    let output = async {
        let mut stdout = std::io::stdout();
        loop {
            match read_frame(&mut reader).await {
                Ok(Some(Frame::Data(data))) => {
                    if stdout
                        .write_all(&data)
                        .and_then(|()| stdout.flush())
                        .is_err()
                    {
                        return 141;
                    }
                }
                Ok(Some(Frame::Exit(code))) => return code,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return 1,
            }
        }
    };
    let input = async {
        while let Some(frame) = pending.recv().await {
            if write_frame(&mut writer, &frame).await.is_err() {
                break;
            }
        }
        std::future::pending::<()>().await;
    };
    let code = tokio::select! {
        code = output => code,
        () = input => 1,
    };
    raw.restore();
    Ok(code)
}
