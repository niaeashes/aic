// repl/spinner — a one-line stderr spinner shown while nothing else is printing
// (waiting for the first token, running a tool call).
//
// The animation runs on its own OS thread so it keeps ticking while the async
// side is blocked on the network. `stop` wakes the thread, waits for it to
// exit, and clears the line, so whatever the caller prints next starts on a
// clean line with no interleaving.
//
// Only animates when stderr is a terminal; otherwise `start` is a no-op so
// redirected output never fills up with `\r` frames.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);

#[derive(Default)]
pub struct Spinner {
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Spinner {
    /// Start animating `label` (replacing any running spinner). The elapsed
    /// time is appended, e.g. `⠹ waiting 3.2s`.
    pub fn start(&mut self, label: impl Into<String>) {
        self.stop();
        if !std::io::stderr().is_terminal() {
            return;
        }
        let label = label.into();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let started = Instant::now();
            let mut err = std::io::stderr();
            for frame in FRAMES.iter().cycle() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let secs = started.elapsed().as_secs_f32();
                // \x1b[2K clears the line so a shorter frame leaves no residue.
                let _ = write!(err, "\r\x1b[2K{frame} {label} {secs:.1}s");
                let _ = err.flush();
                thread::park_timeout(TICK);
            }
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        });
        self.running = Some(Running { stop, handle });
    }

    /// Stop the spinner (if any) and clear its line. Returns once the line is
    /// clean, so the caller can print immediately. Idempotent.
    pub fn stop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop.store(true, Ordering::Relaxed);
            r.handle.thread().unpark();
            let _ = r.handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_without_start_is_noop() {
        let mut s = Spinner::default();
        s.stop();
        s.stop();
    }

    #[test]
    fn start_then_stop_returns() {
        // Under `cargo test` stderr is usually not a TTY, so this mostly checks
        // that start/stop never hang either way.
        let mut s = Spinner::default();
        s.start("waiting");
        s.stop();
        assert!(s.running.is_none());
    }
}
