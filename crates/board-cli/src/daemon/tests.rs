use super::daemon_command;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[test]
fn daemon_child_owns_a_distinct_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("probe-daemon.sh");
    let evidence = dir.path().join("process-group");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s %s %s\\n' \"$$\" \"$(ps -o pgid= -p $$)\" \"$(ps -o pgid= -p $PPID)\" > \"$BOARD_TEST_PROCESS_GROUP\"\nsleep 30\n",
    )
    .expect("write probe");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("chmod probe");

    let log = dir.path().join("daemon.log");
    let mut command = daemon_command(Path::new(&script), &log).expect("build command");
    command.env("BOARD_TEST_PROCESS_GROUP", &evidence);
    let mut child = command.spawn().expect("spawn probe");

    for _ in 0..100 {
        if evidence.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let values = fs::read_to_string(&evidence).expect("read process-group evidence");
    let mut fields = values.split_whitespace();
    let pid: i32 = fields.next().expect("pid").parse().expect("numeric pid");
    let pgid: i32 = fields.next().expect("pgid").parse().expect("numeric pgid");
    let parent_pgid: i32 = fields
        .next()
        .expect("parent pgid")
        .parse()
        .expect("numeric parent pgid");
    assert_eq!(pid, pgid, "daemon child must lead its owned process group");
    assert_ne!(
        pgid, parent_pgid,
        "daemon group must be distinct from its parent"
    );

    child.kill().expect("kill probe");
    child.wait().expect("reap probe");
}
