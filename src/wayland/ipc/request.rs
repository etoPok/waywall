use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) enum Framing {
    UntilEof,
    Line,
}

pub(super) fn request_json(
    socket_path: &Path,
    request: &[u8],
    framing: Framing,
) -> anyhow::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("Cannot connect to compositor socket {:?}", socket_path))?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.write_all(request)?;

    let buf = match framing {
        Framing::UntilEof => {
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut buf = String::new();
            stream.read_to_string(&mut buf)?;
            buf
        }
        Framing::Line => {
            let mut reader = BufReader::new(stream);
            let mut buf = String::new();
            reader.read_line(&mut buf)?;
            buf
        }
    };

    serde_json::from_str(&buf)
        .with_context(|| format!("Invalid JSON response from {:?}", socket_path))
}

pub fn wait_for_shutdown(shutdown: &std::sync::atomic::AtomicBool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while !shutdown.load(std::sync::atomic::Ordering::Acquire)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
