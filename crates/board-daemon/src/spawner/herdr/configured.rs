//! Configured (unmanaged) harnesses: a board-owned pane runs a generated,
//! shell-free startup script handed to it through the `herdr pane run` CLI.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use board_herdr::{HerdrClient, PaneRenameParams};

use super::super::placement::{mark_retryable_placement_race, mark_retryable_runner_race};
use super::super::HerdrLaunchPlan;

/// Injectable bridge for configured harnesses. Keeping the CLI boundary here
/// lets tests verify the exact shell-free invocation.
pub(crate) trait PaneRunner: Send + Sync {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct HerdrCliPaneRunner;

impl PaneRunner for HerdrCliPaneRunner {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()> {
        let herdr_bin = std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "herdr".to_string());
        let status = Command::new(herdr_bin)
            .args(argv)
            .env("HERDR_SOCKET_PATH", socket)
            .status()
            .context("invoking herdr pane run")?;
        if !status.success() {
            bail!("herdr pane run exited with status {status}");
        }
        Ok(())
    }
}

pub(crate) fn launch_configured(
    client: &mut HerdrClient,
    runner: &dyn PaneRunner,
    socket: &Path,
    req: &HerdrLaunchPlan,
    pane_id: &str,
) -> anyhow::Result<()> {
    if req.argv.is_empty() {
        bail!("configured harness has empty argv");
    }
    client
        .pane_rename(&PaneRenameParams {
            pane_id: pane_id.to_string(),
            label: req.name.clone(),
        })
        .map_err(mark_retryable_placement_race)
        .with_context(|| format!("herdr pane.rename {pane_id}"))?;

    let mut script = tempfile::Builder::new()
        .prefix("herdr-board-run-")
        .tempfile()
        .context("creating configured-harness startup script")?;
    let script_path = script.path().to_path_buf();
    let script_text = configured_script(&script_path, &req.argv);
    script
        .write_all(script_text.as_bytes())
        .context("writing configured-harness startup script")?;
    script
        .flush()
        .context("flushing configured-harness startup script")?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
        .context("setting configured-harness startup script mode to 0700")?;
    // Close the writer before the pane executes this file (Linux rejects an
    // open-for-write executable with ETXTBSY). `keep` transfers cleanup to the
    // script after runner success, or back to the daemon after runner failure.
    let (script_file, script_path) = script
        .keep()
        .context("persisting configured-harness startup script")?;
    drop(script_file);

    let runner_argv = vec![
        "pane".to_string(),
        "run".to_string(),
        pane_id.to_string(),
        script_path.to_string_lossy().into_owned(),
    ];
    let run_result = runner
        .run(socket, &runner_argv)
        .map_err(mark_retryable_runner_race)
        .map_err(|error| {
            let message = format!("{error:#}");
            error.context(format!("herdr pane run {pane_id}: {message}"))
        });

    match run_result {
        Ok(()) => {
            // `pane run` only schedules the command; the pane may not have
            // opened the script when the runner returns. Its first command is
            // therefore the sole owner of successful-launch cleanup. No fixed
            // daemon deadline can safely unlink a scheduled-but-not-yet-opened
            // script, and an unbounded sleeping reaper thread is unacceptable.
            // If the pane never opens it, an orphan is the unavoidable side of
            // this scheduling boundary.
            Ok(())
        }
        Err(error) => {
            let remove_result = remove_file_if_exists(&script_path)
                .context("removing configured-harness startup script after runner failure");
            match remove_result {
                Ok(()) => Err(error),
                Err(remove_error) => Err(error.context(format!(
                    "additionally failed to remove startup script: {remove_error:#}"
                ))),
            }
        }
    }
}

pub(crate) fn configured_script(path: &Path, argv: &[String]) -> String {
    let mut script = String::from("#!/bin/sh\nrm -f -- ");
    script.push_str(&posix_quote(&path.to_string_lossy()));
    script.push('\n');
    for arg in argv {
        script.push_str(&posix_quote(arg));
        script.push(' ');
    }
    script.push_str("\nchild_status=$?\n");
    script.push_str("if [ -n \"${BOARD_BIN:-}\" ]; then\n");
    script.push_str("\t\"$BOARD_BIN\" __pane-exited --run-id \"$BOARD_RUN_ID\" || :\n");
    script.push_str("fi\nexit \"$child_status\"\n");
    script
}

pub(crate) fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
