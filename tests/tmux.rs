//! Real-product lifecycle regression. Run explicitly with UT_COMPARE_TMUX_BINARY set.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use ut_compare::adapters::{wait_for_process_count, wait_for_process_count_at_most, AppAdapter};
use ut_compare::config::ContenderConfig;
use ut_compare::process::ProcessSnapshot;

fn tmux_adapter() -> AppAdapter {
    let binary = PathBuf::from(
        std::env::var_os("UT_COMPARE_TMUX_BINARY")
            .expect("set UT_COMPARE_TMUX_BINARY to a known tmux release artifact"),
    );
    let binary = std::fs::canonicalize(binary).unwrap();
    AppAdapter::new(
        ContenderConfig {
            id: "tmux-test".into(),
            display_name: "tmux".into(),
            adapter: "tmux".into(),
            source: binary.parent().unwrap().into(),
            binary,
            environment: BTreeMap::new(),
            assurance: Vec::new(),
        },
        160,
        50,
    )
}

#[test]
#[ignore = "requires a real tmux binary, native Unix sockets and PTYs"]
fn private_servers_sixteen_panes_restart_and_drop_cleanup() {
    let adapter = tmux_adapter();
    // Same session name in separate private sockets must remain independent.
    let mut first = adapter.start(0, false).unwrap().session;
    let second = adapter.start(0, false).unwrap().session;
    assert_ne!(first.root_pid, second.root_pid);
    let first_root = first.workdir.root.clone();
    let second_root = second.workdir.root.clone();
    let second_pid = second.root_pid;
    let mut client = first.attach().unwrap();
    client.drain_for(Duration::from_millis(500)).unwrap();
    for panes in 2..=16 {
        client.send(first.split_sequence()).unwrap();
        client.drain_for(Duration::from_millis(200)).unwrap();
        wait_for_process_count(first.root_pid, panes + 1, Duration::from_secs(5)).unwrap();
        let output = first.status_command().output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().lines().count(),
            panes
        );
    }
    for panes in (1..16).rev() {
        client.send_line("exit").unwrap();
        client.drain_for(Duration::from_millis(200)).unwrap();
        wait_for_process_count_at_most(first.root_pid, panes + 1, Duration::from_secs(5)).unwrap();
    }
    client.send(first.detach_sequence()).unwrap();
    client.drain_for(Duration::from_millis(200)).unwrap();
    client.terminate();
    // Shell history counts as workload state; enlarging the harness config must not count.
    let state_bytes = first.state_bytes();
    let config_path = first.workdir.config.join("tmux-benchmark.conf");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("# additional harness configuration comment\n");
    std::fs::write(config_path, config).unwrap();
    assert_eq!(first.state_bytes(), state_bytes);
    first.stop().unwrap();
    assert!(!first.status_command().output().unwrap().status.success());
    assert!(second.status_command().output().unwrap().status.success());
    first.restart().unwrap();
    let pid = first.root_pid;
    let pane_pids: Vec<_> = ProcessSnapshot::collect()
        .unwrap()
        .descendants_including(pid)
        .into_iter()
        .collect();
    drop(first);
    assert!(!first_root.exists());
    let snapshot = ProcessSnapshot::collect().unwrap();
    assert!(pane_pids
        .iter()
        .all(|pid| !snapshot.processes.contains_key(pid)));
    assert!(snapshot.processes.contains_key(&second_pid));
    drop(second);
    assert!(!second_root.exists());
    assert!(!ProcessSnapshot::collect()
        .unwrap()
        .processes
        .contains_key(&second_pid));
}

#[test]
#[ignore = "requires a real tmux binary and native Unix sockets"]
fn failed_readiness_cleans_the_private_daemon() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;
    let mut adapter = tmux_adapter();
    let wrapper_tree = ut_compare::adapters::IsolatedWorkdir::create("tmux-fail", false).unwrap();
    let wrapper = wrapper_tree.root.join("tmux-failing-probe");
    let pid_file = wrapper_tree.root.join("server.pid");
    // Start the real product, but deliberately make every readiness probe fail.
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
for arg do
    case "$arg" in
        list-panes) exit 1 ;;
        new-session)
            pid=$("$BENCH_REAL_TMUX" "$@") || exit 1
            printf '%s\n' "$pid" > "$BENCH_PID_FILE"
            printf '%s\n' "$pid"
            exit 0 ;;
    esac
done
exec "$BENCH_REAL_TMUX" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    adapter.config.environment.insert(
        "BENCH_REAL_TMUX".into(),
        adapter.config.binary.display().to_string(),
    );
    adapter
        .config
        .environment
        .insert("BENCH_PID_FILE".into(), pid_file.display().to_string());
    adapter.config.binary = wrapper;
    let result = adapter.start(99, false);
    assert!(result.is_err());
    let pid: u32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while ProcessSnapshot::collect()
        .unwrap()
        .processes
        .contains_key(&pid)
    {
        assert!(
            Instant::now() < deadline,
            "failed startup left tmux server {pid} alive"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
