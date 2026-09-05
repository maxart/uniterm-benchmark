use crate::model::HostInfo;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub rss_kib: u64,
    pub cpu_seconds: f64,
    pub comm: String,
    pub args: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessSnapshot {
    pub processes: BTreeMap<u32, ProcessInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct CohortMetrics {
    pub process_count: usize,
    pub root_rss_kib: u64,
    pub cohort_rss_kib: u64,
    pub root_cpu_seconds: f64,
    pub cohort_cpu_seconds: f64,
}

impl ProcessSnapshot {
    pub fn collect() -> Result<Self, String> {
        let output = Command::new("ps")
            .args(["-axo", "pid=,ppid=,rss=,time=,comm=,args="])
            .output()
            .map_err(|error| format!("could not run ps: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ps failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut processes = BTreeMap::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(ppid), Some(rss), Some(cpu), Some(comm)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let (Ok(pid), Ok(ppid), Ok(rss_kib), Some(cpu_seconds)) = (
                pid.parse::<u32>(),
                ppid.parse::<u32>(),
                rss.parse::<u64>(),
                parse_ps_time(cpu),
            ) else {
                continue;
            };
            processes.insert(
                pid,
                ProcessInfo {
                    pid,
                    ppid,
                    rss_kib,
                    cpu_seconds,
                    comm: comm.to_owned(),
                    args: fields.collect::<Vec<_>>().join(" "),
                },
            );
        }
        overlay_proc_cpu_times(&mut processes);
        Ok(Self { processes })
    }

    pub fn descendants_including(&self, root: u32) -> BTreeSet<u32> {
        let mut result = BTreeSet::from([root]);
        loop {
            let previous = result.len();
            for process in self.processes.values() {
                if result.contains(&process.ppid) {
                    result.insert(process.pid);
                }
            }
            if result.len() == previous {
                return result;
            }
        }
    }

    pub fn metrics(&self, root: u32) -> Option<CohortMetrics> {
        self.metrics_many(&[root])
    }

    pub fn metrics_many(&self, roots: &[u32]) -> Option<CohortMetrics> {
        if roots.is_empty() || roots.iter().any(|root| !self.processes.contains_key(root)) {
            return None;
        }
        let mut cohort = BTreeSet::new();
        for root in roots {
            cohort.extend(self.descendants_including(*root));
        }
        let mut metrics = CohortMetrics {
            process_count: cohort.len(),
            ..CohortMetrics::default()
        };
        for root in roots {
            let process = self.processes.get(root)?;
            metrics.root_rss_kib += process.rss_kib;
            metrics.root_cpu_seconds += process.cpu_seconds;
        }
        for pid in cohort {
            if let Some(process) = self.processes.get(&pid) {
                metrics.cohort_rss_kib += process.rss_kib;
                metrics.cohort_cpu_seconds += process.cpu_seconds;
            }
        }
        Some(metrics)
    }

    pub fn new_pids_matching(&self, before: &Self, needle: &Path, arg: &str) -> Vec<u32> {
        let filename = needle
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        self.processes
            .values()
            .filter(|process| !before.processes.contains_key(&process.pid))
            .filter(|process| {
                (process.comm == filename || process.args.contains(&needle.to_string_lossy()[..]))
                    && process.args.contains(arg)
            })
            .map(|process| process.pid)
            .collect()
    }
}

/// Linux `ps` reports cumulative CPU time in whole seconds, which cannot resolve idle CPU inside a
/// 30 s window. When `/proc/<pid>/stat` is readable, replace the coarse value with the kernel's
/// utime+stime tick counter (usually 10 ms resolution). Other platforms keep the portable `ps` value.
#[cfg(target_os = "linux")]
fn overlay_proc_cpu_times(processes: &mut BTreeMap<u32, ProcessInfo>) {
    // SAFETY: sysconf has no preconditions and only returns a value.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return;
    }
    for process in processes.values_mut() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", process.pid)) else {
            continue;
        };
        if let Some(seconds) = parse_proc_stat_cpu_seconds(&stat, ticks as f64) {
            process.cpu_seconds = seconds;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn overlay_proc_cpu_times(_processes: &mut BTreeMap<u32, ProcessInfo>) {}

/// Parses utime and stime (fields 14 and 15) from a `/proc/<pid>/stat` line. The command name
/// can contain spaces and parentheses, so fields are located after the final `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat_cpu_seconds(stat: &str, ticks_per_second: f64) -> Option<f64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let utime: f64 = fields.nth(11)?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    Some((utime + stime) / ticks_per_second)
}

pub fn cpu_time_source() -> String {
    if cfg!(target_os = "linux") && Path::new("/proc/self/stat").exists() {
        // SAFETY: sysconf has no preconditions and only returns a value.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 {
            return format!(
                "/proc/<pid>/stat utime+stime ({:.0} ms ticks); ps supplies PID, RSS, and tree",
                1000.0 / ticks as f64
            );
        }
    }
    "ps cumulative time (platform resolution; whole seconds on procps)".into()
}

pub fn sample_cohort(
    root: u32,
    duration: Duration,
    interval: Duration,
) -> Result<(Vec<CohortMetrics>, Duration), String> {
    sample_cohorts(&[root], duration, interval)
}

pub fn sample_cohorts(
    roots: &[u32],
    duration: Duration,
    interval: Duration,
) -> Result<(Vec<CohortMetrics>, Duration), String> {
    let started = Instant::now();
    let mut samples = Vec::new();
    loop {
        let snapshot = ProcessSnapshot::collect()?;
        let Some(metrics) = snapshot.metrics_many(roots) else {
            return Err(format!(
                "one of root processes {roots:?} exited during sampling"
            ));
        };
        samples.push(metrics);
        if started.elapsed() >= duration {
            return Ok((samples, started.elapsed()));
        }
        std::thread::sleep(interval.min(duration.saturating_sub(started.elapsed())));
    }
}

pub fn cpu_percent(samples: &[CohortMetrics], elapsed: Duration, cohort: bool) -> Option<f64> {
    let first = samples.first()?;
    let last = samples.last()?;
    let delta = if cohort {
        last.cohort_cpu_seconds - first.cohort_cpu_seconds
    } else {
        last.root_cpu_seconds - first.root_cpu_seconds
    };
    Some((delta.max(0.0) / elapsed.as_secs_f64()) * 100.0)
}

pub fn run_output(command: &mut Command) -> Result<Output, String> {
    command
        .output()
        .map_err(|error| format!("could not run {:?}: {error}", command.get_program()))
}

pub fn checked_output(command: &mut Command) -> Result<String, String> {
    let output = run_output(command)?;
    if !output.status.success() {
        return Err(format!(
            "command {:?} failed with {}: {}",
            command.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn host_info() -> HostInfo {
    let os = std::env::consts::OS.to_owned();
    let architecture = std::env::consts::ARCH.to_owned();
    let kernel = checked_output(Command::new("uname").arg("-sr")).unwrap_or_else(|_| os.clone());
    let hostname =
        checked_output(&mut Command::new("hostname")).unwrap_or_else(|_| "unknown".into());
    let logical_cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let rustc = checked_output(Command::new("rustc").arg("--version")).ok();
    let wsl = detect_wsl();
    HostInfo {
        os,
        architecture,
        kernel,
        hostname,
        logical_cpus,
        rustc,
        wsl,
        git_dirty_policy: "recorded, never silently normalized".into(),
        cpu_time_source: cpu_time_source(),
    }
}

fn detect_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|value| value.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

fn parse_ps_time(value: &str) -> Option<f64> {
    let (days, clock) = if let Some((days, rest)) = value.split_once('-') {
        (days.parse::<f64>().ok()?, rest)
    } else {
        (0.0, value)
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let seconds = match fields.as_slice() {
        [minutes, seconds] => minutes.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?,
        [hours, minutes, seconds] => {
            hours.parse::<f64>().ok()? * 3600.0
                + minutes.parse::<f64>().ok()? * 60.0
                + seconds.parse::<f64>().ok()?
        }
        _ => return None,
    };
    Some(days * 86_400.0 + seconds)
}

pub fn directory_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_file() {
            total += metadata.len();
        } else if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_and_macos_cpu_time() {
        assert_eq!(parse_ps_time("00:01"), Some(1.0));
        assert_eq!(parse_ps_time("01:02:03"), Some(3723.0));
        assert_eq!(parse_ps_time("2-01:02:03"), Some(176_523.0));
        assert_eq!(parse_ps_time("0:00.25"), Some(0.25));
    }

    #[test]
    fn proc_stat_cpu_fields_follow_the_last_paren() {
        let stat = "1234 (my (odd) name) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 1 0 5000 1000000 200 18446744073709551615";
        assert_eq!(parse_proc_stat_cpu_seconds(stat, 100.0), Some(3.0));
        assert_eq!(parse_proc_stat_cpu_seconds("garbage", 100.0), None);
    }

    #[test]
    fn descendants_are_transitive() {
        let snapshot = ProcessSnapshot {
            processes: BTreeMap::from([
                (1, process(1, 0)),
                (2, process(2, 1)),
                (3, process(3, 2)),
                (4, process(4, 9)),
            ]),
        };
        assert_eq!(snapshot.descendants_including(1), BTreeSet::from([1, 2, 3]));
    }

    fn process(pid: u32, ppid: u32) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            rss_kib: 1,
            cpu_seconds: 0.0,
            comm: "test".into(),
            args: String::new(),
        }
    }
}
