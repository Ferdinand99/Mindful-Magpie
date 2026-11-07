//! Capturing what a test binary logs, to check that no secret reaches a log.
//!
//! The subscriber also bridges the `log` records of reqwest and hyper, so the
//! capture holds the HTTP client's own lines. [`client_lines_seen`] says
//! whether it does: the stub server (hyper-util's server side, axum) logs too,
//! and its lines must not stand in for the client's.

use std::io::Write;
use std::sync::{Arc, Mutex, Once, OnceLock};

#[derive(Clone)]
pub struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The capture; the first call installs it as this binary's subscriber.
pub fn buffer() -> &'static LogBuffer {
    static BUFFER: OnceLock<LogBuffer> = OnceLock::new();
    static INSTALL: Once = Once::new();
    let buffer = BUFFER.get_or_init(|| LogBuffer(Arc::new(Mutex::new(Vec::new()))));
    INSTALL.call_once(|| {
        let writer = buffer.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .expect("no other subscriber in this test binary");
    });
    buffer
}

/// Everything logged so far.
pub fn logged() -> String {
    String::from_utf8_lossy(&buffer().0.lock().unwrap()).into_owned()
}

/// Whether `log` holds lines of the HTTP *client* (reqwest, or hyper-util's
/// client side), not only the stub server's.
pub fn client_lines_seen(log: &str) -> bool {
    log.contains("reqwest::") || log.contains("hyper_util::client")
}
