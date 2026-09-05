#![cfg(unix)]

use crate::screen::Screen;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct PtyChild {
    master: File,
    child: Child,
    stripped: Vec<u8>,
    raw_bytes: u64,
    screen: Screen,
}

pub struct MarkerObservation {
    pub elapsed: Duration,
    pub raw_bytes: u64,
}

impl PtyChild {
    pub fn spawn(command: &mut Command, cols: u16, rows: u16) -> Result<Self, String> {
        let mut master_fd: RawFd = -1;
        let mut slave_fd: RawFd = -1;
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes both descriptors and only reads the supplied winsize.
        if unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } < 0
        {
            return Err(format!(
                "openpty failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let stdin_fd = duplicate(slave_fd)?;
        let stdout_fd = duplicate(slave_fd)?;
        let stderr_fd = duplicate(slave_fd)?;
        // SAFETY: each descriptor is uniquely owned by the File after this point.
        command.stdin(Stdio::from(unsafe { File::from_raw_fd(stdin_fd) }));
        command.stdout(Stdio::from(unsafe { File::from_raw_fd(stdout_fd) }));
        command.stderr(Stdio::from(unsafe { File::from_raw_fd(stderr_fd) }));
        // SAFETY: pre_exec runs in the child. setsid and TIOCSCTTY establish the PTY as the
        // controlling terminal before exec. No allocation or lock-taking occurs in the closure.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|error| {
            // SAFETY: the parent still owns both original openpty descriptors here.
            unsafe {
                libc::close(master_fd);
                libc::close(slave_fd);
            }
            format!("could not spawn PTY child: {error}")
        })?;
        // SAFETY: the slave is no longer needed by the parent and is closed exactly once.
        unsafe {
            libc::close(slave_fd);
        }
        set_nonblocking(master_fd)?;
        // SAFETY: master_fd is now uniquely owned by master.
        let master = unsafe { File::from_raw_fd(master_fd) };
        Ok(Self {
            master,
            child,
            stripped: Vec::new(),
            raw_bytes: 0,
            screen: Screen::new(cols, rows),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Printable text observed since the last `clear_observation`, with escape sequences removed.
    pub fn observed_text(&self) -> &[u8] {
        &self.stripped
    }

    /// The modelled visible screen, replayed from every byte the client has written so far.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let started = Instant::now();
        let mut offset = 0;
        while offset < bytes.len() {
            match self.master.write(&bytes[offset..]) {
                Ok(0) => return Err("PTY accepted zero input bytes".into()),
                Ok(written) => offset += written,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if started.elapsed() > Duration::from_secs(2) {
                        return Err("timed out writing to PTY".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("PTY write failed: {error}")),
            }
        }
        Ok(())
    }

    pub fn send_line(&mut self, text: &str) -> Result<(), String> {
        self.send(text.as_bytes())?;
        self.send(b"\r")
    }

    pub fn drain_for(&mut self, duration: Duration) -> Result<u64, String> {
        let deadline = Instant::now() + duration;
        let before = self.raw_bytes;
        while Instant::now() < deadline {
            self.read_available()?;
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(self.raw_bytes - before)
    }

    pub fn clear_observation(&mut self) -> Result<(), String> {
        self.read_available()?;
        self.stripped.clear();
        self.raw_bytes = 0;
        Ok(())
    }

    pub fn read_until_text(
        &mut self,
        marker: &str,
        timeout: Duration,
    ) -> Result<MarkerObservation, String> {
        let started = Instant::now();
        let marker = marker.as_bytes();
        loop {
            self.read_available()?;
            if contains_subslice(&self.stripped, marker) {
                return Ok(MarkerObservation {
                    elapsed: started.elapsed(),
                    raw_bytes: self.raw_bytes,
                });
            }
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "PTY child exited with {status} before marker {:?}; output tail: {}",
                    String::from_utf8_lossy(marker),
                    String::from_utf8_lossy(tail(&self.stripped, 500))
                ));
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {:.1}s waiting for marker {:?}; output tail: {}",
                    timeout.as_secs_f64(),
                    String::from_utf8_lossy(marker),
                    String::from_utf8_lossy(tail(&self.stripped, 500))
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Waits until `text` is visible on the modelled screen. Use this instead of
    /// `read_until_text` when the same screen cells may already hold most of the text: a
    /// damage-based renderer then emits only the changed cells, so the raw stream never contains
    /// the whole string even though it is fully visible.
    pub fn read_until_screen(&mut self, text: &str, timeout: Duration) -> Result<Duration, String> {
        let started = Instant::now();
        loop {
            self.read_available()?;
            if self.screen.count(text) > 0 {
                return Ok(started.elapsed());
            }
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "PTY child exited with {status} before {text:?} became visible\n{}",
                    self.screen.dump()
                ));
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {:.1}s waiting for {text:?} to become visible\n{}",
                    timeout.as_secs_f64(),
                    self.screen.dump()
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a live PTY descriptor and size is fully initialized.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(format!(
                "PTY resize failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.screen.resize(cols, rows);
        Ok(())
    }

    pub fn terminate(&mut self) {
        // Kill the PTY child's process group first so helper processes cannot outlive a benchmark.
        // SAFETY: negative pid targets the process group created by setsid in spawn.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: same process group, escalated only after the grace period.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    fn read_available(&mut self) -> Result<(), String> {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => {
                    self.raw_bytes += read as u64;
                    self.screen.feed(&buffer[..read]);
                    strip_terminal_sequences(&buffer[..read], &mut self.stripped);
                    if self.stripped.len() > 16 * 1024 * 1024 {
                        let keep_from = self.stripped.len() - 8 * 1024 * 1024;
                        self.stripped.drain(..keep_from);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()),
                Err(error) => return Err(format!("PTY read failed: {error}")),
            }
        }
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.terminate();
        }
    }
}

use std::os::fd::AsRawFd;

fn duplicate(fd: RawFd) -> Result<RawFd, String> {
    // SAFETY: fd is live and dup returns a new independently-owned descriptor.
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        Err(format!("dup failed: {}", std::io::Error::last_os_error()))
    } else {
        Ok(duplicated)
    }
}

fn set_nonblocking(fd: RawFd) -> Result<(), String> {
    // SAFETY: fcntl reads and updates flags on a live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(format!("fcntl failed: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn tail(value: &[u8], max: usize) -> &[u8] {
    &value[value.len().saturating_sub(max)..]
}

/// Removes common CSI, OSC, DCS and single-character escape sequences. The benchmark markers use
/// printable ASCII, so this intentionally favors resilient marker detection over terminal emulation.
fn strip_terminal_sequences(input: &[u8], output: &mut Vec<u8>) {
    let mut index = 0;
    while index < input.len() {
        if input[index] != 0x1b {
            if input[index] >= 0x20 || matches!(input[index], b'\n' | b'\r' | b'\t') {
                output.push(input[index]);
            }
            index += 1;
            continue;
        }
        index += 1;
        let Some(&kind) = input.get(index) else {
            break;
        };
        index += 1;
        match kind {
            b'[' => {
                while let Some(&byte) = input.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' | b'P' | b'_' | b'^' => {
                while index < input.len() {
                    if input[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if input[index] == 0x1b && input.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_around_markers() {
        let mut output = Vec::new();
        strip_terminal_sequences(b"\x1b[2Jbefore\x1b]0;title\x07MARK\x1b[0m", &mut output);
        assert_eq!(output, b"beforeMARK");
    }

    #[test]
    fn pty_round_trip_observes_shell_output() {
        let mut command = Command::new("/bin/sh");
        let mut child = PtyChild::spawn(&mut command, 80, 24).unwrap();
        child.drain_for(Duration::from_millis(50)).unwrap();
        child.clear_observation().unwrap();
        child.send_line("printf 'PTY_MARKER\\n'").unwrap();
        let observed = child
            .read_until_text("PTY_MARKER", Duration::from_secs(2))
            .unwrap();
        assert!(observed.raw_bytes > 0);
        child.drain_for(Duration::from_millis(50)).unwrap();
        // The command echo and its output both carry the marker; the point is that the screen
        // model observed it end to end.
        assert!(
            child.screen().count("PTY_MARKER") >= 1,
            "{}",
            child.screen().dump()
        );
    }
}
