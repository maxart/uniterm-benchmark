use crate::config::ContenderConfig;
use crate::process::{checked_output, ProcessSnapshot};
use crate::pty::PtyChild;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AppAdapter {
    pub config: ContenderConfig,
    /// Terminal geometry every attached client and headless render surface must use.
    pub cols: u16,
    pub rows: u16,
}

pub struct Session {
    adapter: AppAdapter,
    pub name: String,
    pub workdir: IsolatedWorkdir,
    pub root_pid: u32,
    server_child: Option<Child>,
    stopped: bool,
}

pub struct StartedSession {
    pub session: Session,
    pub startup_elapsed: Duration,
}

pub struct IsolatedWorkdir {
    pub root: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
    keep: bool,
}

impl IsolatedWorkdir {
    pub fn create(label: &str, keep: bool) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::OnceLock;
        static NEXT_NONCE: OnceLock<AtomicU64> = OnceLock::new();

        let label: String = label
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        // A clock read alone can repeat across concurrent calls, especially on macOS.
        let counter = NEXT_NONCE.get_or_init(|| {
            AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            )
        });
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let mut root = None;
        for _ in 0..128 {
            let candidate = std::env::temp_dir().join(format!(
                "utc-{}-{}-{:08x}",
                label,
                std::process::id(),
                counter.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff
            ));
            // Never adopt an existing directory or follow a symlink. Retry stale names.
            match builder.create(&candidate) {
                Ok(()) => {
                    root = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("could not create private workdir: {error}")),
            }
        }
        let root = root.ok_or("could not create a unique private workdir")?;
        let home = root.join("home");
        let runtime = root.join("runtime");
        let state = root.join("state");
        let config = root.join("config");
        let mut workdir = Self {
            root,
            home,
            runtime,
            state,
            config,
            keep: false,
        };
        // Construct the cleanup guard before creating children so partial failures clean up.
        for path in [
            &workdir.home,
            &workdir.runtime,
            &workdir.state,
            &workdir.config,
        ] {
            builder
                .create(path)
                .map_err(|error| format!("could not create private workdir child: {error}"))?;
        }
        workdir.keep = keep;
        Ok(workdir)
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HOME".into(), self.home.display().to_string()),
            ("XDG_RUNTIME_DIR".into(), self.runtime.display().to_string()),
            ("XDG_STATE_HOME".into(), self.state.display().to_string()),
            ("XDG_CONFIG_HOME".into(), self.config.display().to_string()),
            (
                "XDG_CACHE_HOME".into(),
                self.root.join("cache").display().to_string(),
            ),
            ("SHELL".into(), "/bin/sh".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("NO_COLOR".into(), "1".into()),
            ("LC_ALL".into(), "C".into()),
        ])
    }
}

impl Drop for IsolatedWorkdir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl AppAdapter {
    pub fn new(config: ContenderConfig, cols: u16, rows: u16) -> Self {
        Self { config, cols, rows }
    }

    pub fn start(&self, sequence: usize, keep_workdir: bool) -> Result<StartedSession, String> {
        let session_name = format!("utc-{}-{sequence}", self.config.id);
        let workdir = IsolatedWorkdir::create(&self.config.id, keep_workdir)?;
        self.prepare_config(&workdir)?;
        let (root_pid, server_child, startup_elapsed) = self.launch(&workdir, &session_name)?;
        Ok(StartedSession {
            session: Session {
                adapter: self.clone(),
                name: session_name,
                workdir,
                root_pid,
                server_child,
                stopped: false,
            },
            startup_elapsed,
        })
    }

    /// Starts (or restarts) the product server for `session_name` inside `workdir`.
    ///
    /// Startup is timed identically for both products: from launching the product's own start
    /// command until the same kind of readiness probe (a pane listing through the product socket)
    /// succeeds. Server PID discovery happens outside the timed window.
    fn launch(
        &self,
        workdir: &IsolatedWorkdir,
        session_name: &str,
    ) -> Result<(u32, Option<Child>, Duration), String> {
        let before = ProcessSnapshot::collect()?;
        let started = Instant::now();
        match self.config.adapter.as_str() {
            "uniterm" => {
                let mut command = self.command(workdir);
                command.args(["workspace", "new", "-d", session_name]);
                let output = checked_output(&mut command)?;
                if !output.contains("started") {
                    return Err(format!(
                        "Uniterm did not report startup readiness: {output}"
                    ));
                }
                self.wait_ready(workdir, session_name, None)?;
                let startup_elapsed = started.elapsed();
                let root = self.discover_uniterm_server(&before, session_name)?;
                Ok((root, None, startup_elapsed))
            }
            "herdr" => {
                let mut command = self.command(workdir);
                command
                    .env("HERDR_SESSION", session_name)
                    .arg("server")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut child = command
                    .spawn()
                    .map_err(|error| format!("could not start Herdr server: {error}"))?;
                let root = child.id();
                self.wait_ready(workdir, session_name, Some(&mut child))?;
                Ok((root, Some(child), started.elapsed()))
            }
            "tmux" => {
                let mut command = self.command(workdir);
                command.args([
                    "new-session",
                    "-d",
                    "-s",
                    session_name,
                    "-x",
                    &self.cols.to_string(),
                    "-y",
                    &self.rows.to_string(),
                    "-P",
                    "-F",
                    "#{pid}",
                ]);
                // The daemon reports its own PID; never discover or address a user's server.
                let outcome = (|| {
                    let output = checked_output(&mut command)?;
                    self.wait_ready(workdir, session_name, None)?;
                    let elapsed = started.elapsed();
                    let pid = parse_server_pid(&output)?;
                    Ok((pid, None, elapsed))
                })();
                if outcome.is_err() {
                    let _ = self.command(workdir).arg("kill-server").output();
                }
                outcome
            }
            other => Err(format!("unsupported adapter {other}")),
        }
    }

    /// Files the harness writes into the isolated tree; they are not product state.
    pub fn harness_files(&self, workdir: &IsolatedWorkdir) -> Vec<PathBuf> {
        match self.config.adapter.as_str() {
            "herdr" => vec![workdir.config.join("herdr-benchmark.toml")],
            "tmux" => vec![workdir.config.join("tmux-benchmark.conf")],
            _ => Vec::new(),
        }
    }

    fn prepare_config(&self, workdir: &IsolatedWorkdir) -> Result<(), String> {
        if self.config.adapter == "herdr" {
            let path = workdir.config.join("herdr-benchmark.toml");
            // Onboarding is disabled so the client opens directly on a terminal; the version and
            // agent-manifest checks are the only default network activity and are disabled for
            // timing; the headless grid matches the attached geometry so the detached server does
            // not render at a different size from Uniterm's last-attached client size.
            std::fs::write(&path, herdr_benchmark_config(self.cols, self.rows))
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        } else if self.config.adapter == "tmux" {
            let path = workdir.config.join("tmux-benchmark.conf");
            std::fs::write(&path, tmux_benchmark_config())
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        }
        Ok(())
    }

    fn discover_uniterm_server(
        &self,
        before: &ProcessSnapshot,
        session_name: &str,
    ) -> Result<u32, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let after = ProcessSnapshot::collect()?;
            let mut candidates = after.new_pids_matching(before, &self.config.binary, session_name);
            candidates.retain(|pid| {
                after
                    .processes
                    .get(pid)
                    .is_some_and(|process| process.args.contains("serve"))
            });
            if let Some(pid) = candidates.into_iter().next() {
                return Ok(pid);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "could not discover Uniterm server process for {session_name}"
                ));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    fn wait_ready(
        &self,
        workdir: &IsolatedWorkdir,
        session_name: &str,
        mut child: Option<&mut Child>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let mut command = self.pane_list_command(workdir, session_name);
            if command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
            {
                return Ok(());
            }
            if let Some(child) = child.as_deref_mut() {
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    return Err(format!("server exited during startup with {status}"));
                }
            }
            if Instant::now() >= deadline {
                return Err("server did not become ready within 8 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Lists the session's panes through the product socket. Both products exit non-zero when
    /// the server is not reachable, which makes this the readiness probe and the control-latency
    /// command for both contenders. (`ut workspace list` reads the local catalog and succeeds
    /// without a server, so it is not a readiness signal.)
    pub fn pane_list_command(&self, workdir: &IsolatedWorkdir, session_name: &str) -> Command {
        let mut command = self.command(workdir);
        match self.config.adapter.as_str() {
            "uniterm" => {
                command.args(["pane", "list", "-w", session_name]);
            }
            "herdr" => {
                command
                    .env("HERDR_SESSION", session_name)
                    .args(["pane", "list"]);
            }
            "tmux" => {
                command.args(["list-panes", "-t", session_name]);
            }
            _ => unreachable!(),
        }
        command
    }

    pub fn command(&self, workdir: &IsolatedWorkdir) -> Command {
        let mut command = Command::new(&self.config.binary);
        command.current_dir(&workdir.root).env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for (key, value) in workdir.environment() {
            command.env(key, value);
        }
        for (key, value) in &self.config.environment {
            command.env(key, value);
        }
        if self.config.adapter == "herdr" {
            command.env(
                "HERDR_CONFIG_PATH",
                workdir.config.join("herdr-benchmark.toml"),
            );
        } else if self.config.adapter == "tmux" {
            command.arg("-S").arg(workdir.runtime.join("tmux.sock"));
            command
                .arg("-f")
                .arg(workdir.config.join("tmux-benchmark.conf"));
        }
        command
    }
}

pub fn herdr_benchmark_config(cols: u16, rows: u16) -> String {
    format!(
        "onboarding = false\n\n[update]\nversion_check = false\nmanifest_check = false\n\n[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n\n[server]\nheadless_cols = {cols}\nheadless_rows = {rows}\n"
    )
}

fn parse_server_pid(output: &str) -> Result<u32, String> {
    output
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| format!("invalid tmux server PID: {output:?}"))
}

fn tmux_benchmark_config() -> &'static str {
    // A full-window split followed by tiling avoids repeatedly halving the active pane until
    // it is too small. Both commands run through a normal prefix key, outside core timings.
    // Keep status, history, redraw and escape timers at the product defaults.
    "set -g default-shell /bin/sh\n\
     set -g default-command 'exec /bin/sh'\n\
     set -g default-terminal xterm-256color\n\
     bind-key % split-window -h -f \\; select-layout tiled\n"
}

impl Session {
    pub fn attach(&self) -> Result<PtyChild, String> {
        self.attach_with(self.adapter.cols, self.adapter.rows)
    }

    /// Attaches a client at an explicit geometry (used for additional multi-client attaches).
    pub fn attach_with(&self, cols: u16, rows: u16) -> Result<PtyChild, String> {
        let mut command = self.adapter.command(&self.workdir);
        match self.adapter.config.adapter.as_str() {
            "uniterm" => {
                command.args(["workspace", "switch", &self.name]);
            }
            "herdr" => {
                command
                    .env("HERDR_SESSION", &self.name)
                    .args(["--session", &self.name]);
            }
            "tmux" => {
                command.args(["attach-session", "-t", &self.name]);
            }
            _ => unreachable!(),
        }
        PtyChild::spawn(&mut command, cols, rows)
    }

    /// Fresh CLI process that lists panes through the product socket; the same semantic
    /// operation is used for both contenders.
    pub fn status_command(&self) -> Command {
        self.adapter.pane_list_command(&self.workdir, &self.name)
    }

    pub fn split_sequence(&self) -> &'static [u8] {
        match self.adapter.config.adapter.as_str() {
            "uniterm" => b"\x01%",
            "herdr" => b"\x02v",
            "tmux" => b"\x02%",
            _ => unreachable!(),
        }
    }

    pub fn detach_sequence(&self) -> &'static [u8] {
        match self.adapter.config.adapter.as_str() {
            "uniterm" => b"\x01d",
            "herdr" => b"\x02q",
            "tmux" => b"\x02d",
            _ => unreachable!(),
        }
    }

    pub fn stop(&mut self) -> Result<Duration, String> {
        if self.stopped {
            return Ok(Duration::ZERO);
        }
        let started = Instant::now();
        let mut command = self.adapter.command(&self.workdir);
        match self.adapter.config.adapter.as_str() {
            "uniterm" => {
                command.args(["workspace", "stop", &self.name]);
            }
            "herdr" => {
                command
                    .env("HERDR_SESSION", &self.name)
                    .args(["server", "stop"]);
            }
            "tmux" => {
                command.arg("kill-server");
            }
            _ => unreachable!(),
        }
        let output = command
            .output()
            .map_err(|error| format!("could not stop server: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "server stop failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(child) = &mut self.server_child {
                if child
                    .try_wait()
                    .map_err(|error| format!("could not reap server: {error}"))?
                    .is_some()
                {
                    break;
                }
            }
            let alive = ProcessSnapshot::collect()
                .ok()
                .is_some_and(|snapshot| snapshot.processes.contains_key(&self.root_pid));
            if !alive {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "server {} did not exit within 5 seconds",
                    self.root_pid
                ));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if let Some(child) = &mut self.server_child {
            let _ = child.wait();
        }
        self.stopped = true;
        Ok(started.elapsed())
    }

    /// Restarts a stopped session in the same isolated tree using the product's own start
    /// command, timed until the pane-listing probe succeeds exactly like the first start.
    pub fn restart(&mut self) -> Result<Duration, String> {
        if !self.stopped {
            return Err("restart requires a stopped session".into());
        }
        let (root_pid, server_child, elapsed) = self.adapter.launch(&self.workdir, &self.name)?;
        self.root_pid = root_pid;
        self.server_child = server_child;
        self.stopped = false;
        Ok(elapsed)
    }

    /// Bytes the product persisted inside its private HOME/XDG tree. Files the harness itself
    /// writes there (the Herdr and tmux benchmark configs) are excluded so the
    /// context metric reflects product persistence only.
    pub fn state_bytes(&self) -> u64 {
        let workdir = &self.workdir;
        let product: u64 = [
            &workdir.home,
            &workdir.runtime,
            &workdir.state,
            &workdir.config,
            &workdir.root.join("cache"),
        ]
        .into_iter()
        .map(|path| crate::process::directory_bytes(path))
        .sum();
        let harness: u64 = self
            .adapter
            .harness_files(workdir)
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .sum();
        product.saturating_sub(harness)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop();
        }
        if let Some(child) = &mut self.server_child {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

pub fn executable_version(config: &ContenderConfig) -> Result<String, String> {
    let mut command = Command::new(&config.binary);
    command.arg(if config.adapter == "tmux" {
        "-V"
    } else {
        "--version"
    });
    checked_output(&mut command)
}

pub fn command_latency(
    mut make: impl FnMut() -> Command,
    iterations: usize,
) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let output = make()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("command latency probe failed: {error}"))?;
        if !output.success() {
            return Err(format!("command latency probe exited with {output}"));
        }
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    Ok(samples)
}

pub fn wait_for_process_count(root: u32, count: usize, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = ProcessSnapshot::collect()?;
        if snapshot.descendants_including(root).len() >= count {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("process cohort did not reach {count} members"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the root's cohort has shrunk to at most `count` processes.
pub fn wait_for_process_count_at_most(
    root: u32,
    count: usize,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = ProcessSnapshot::collect()?;
        if snapshot.descendants_including(root).len() <= count {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("process cohort did not shrink to {count} members"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn path_is_release_binary(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "release")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_pid_rejects_empty_invalid_and_unsafe_roots() {
        assert_eq!(parse_server_pid(" 1234\n").unwrap(), 1234);
        for output in ["", "0", "1", "-1", "123\n456", "started", "4294967296"] {
            assert!(parse_server_pid(output).is_err(), "{output:?}");
        }
    }

    #[test]
    fn tmux_commands_are_isolated_and_readiness_lists_panes() {
        let workdir = IsolatedWorkdir::create("tmux-test", false).unwrap();
        let adapter = AppAdapter::new(
            ContenderConfig {
                id: "tmux".into(),
                display_name: "tmux".into(),
                adapter: "tmux".into(),
                binary: "/unused/tmux".into(),
                source: "/unused/source".into(),
                environment: BTreeMap::new(),
                assurance: Vec::new(),
            },
            160,
            50,
        );
        let command = adapter.pane_list_command(&workdir, "test-session");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-S".into(),
                workdir.runtime.join("tmux.sock").display().to_string(),
                "-f".into(),
                workdir
                    .config
                    .join("tmux-benchmark.conf")
                    .display()
                    .to_string(),
                "list-panes".into(),
                "-t".into(),
                "test-session".into()
            ]
        );
        let env: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("HOME")),
            Some(&Some(workdir.home.as_os_str()))
        );
        assert!(!env.contains_key(std::ffi::OsStr::new("TMUX")));
    }

    #[test]
    fn herdr_config_disables_network_and_pins_headless_geometry() {
        let config = herdr_benchmark_config(160, 50);
        let parsed: toml::Value = toml::from_str(&config).unwrap();
        assert_eq!(parsed["onboarding"].as_bool(), Some(false));
        assert_eq!(parsed["update"]["version_check"].as_bool(), Some(false));
        assert_eq!(parsed["update"]["manifest_check"].as_bool(), Some(false));
        assert_eq!(parsed["server"]["headless_cols"].as_integer(), Some(160));
        assert_eq!(parsed["server"]["headless_rows"].as_integer(), Some(50));
        assert_eq!(
            parsed["terminal"]["default_shell"].as_str(),
            Some("/bin/sh")
        );
    }
}
