//! Configured (unmanaged) harnesses: a board-owned pane runs a generated,
//! shell-free startup script handed to it through the `herdr pane run` CLI.
//!
//! `herdr pane run <PANE_ID> <COMMAND>...` (verified with `herdr pane run
//! --help` on Herdr 0.9.x; the socket schema has no `pane.run` method) queues
//! the COMMAND words as one line of text plus Enter into the pane's
//! interactive shell. That shell re-parses the line, so argv boundaries are
//! never trusted to it: the exact argv lives in a generated script and the
//! typed line only names that script.
//!
//! - **unix**: an extensionless, mode-0700 `#!/bin/sh` script, typed as its
//!   bare path.
//! - **Windows**: a UTF-8 (BOM) `.ps1` script, typed as
//!   `pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "<path>"`
//!   (`powershell.exe` when `pwsh` does not resolve). That line parses the
//!   same in a pwsh, Windows PowerShell, cmd or sh pane shell.

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
        // `exec::command` is the identity on unix; on Windows it applies
        // PATHEXT so a `herdr.cmd` shim resolves like it would in a shell.
        let status = board_core::exec::command(&herdr_bin)
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

/// File-name suffix of the generated startup script: PowerShell's `-File`
/// only accepts a `.ps1` path.
#[cfg(unix)]
const SCRIPT_SUFFIX: &str = "";
#[cfg(windows)]
const SCRIPT_SUFFIX: &str = ".ps1";

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
    let argv = script_argv(&req.argv)?;
    client
        .pane_rename(&PaneRenameParams {
            pane_id: pane_id.to_string(),
            label: req.name.clone(),
        })
        .map_err(mark_retryable_placement_race)
        .with_context(|| format!("herdr pane.rename {pane_id}"))?;

    let mut script = tempfile::Builder::new()
        .prefix("herdr-board-run-")
        .suffix(SCRIPT_SUFFIX)
        .tempfile()
        .context("creating configured-harness startup script")?;
    let script_path = script.path().to_path_buf();
    let script_text = startup_script(&script_path, &argv);
    script
        .write_all(script_text.as_bytes())
        .context("writing configured-harness startup script")?;
    script
        .flush()
        .context("flushing configured-harness startup script")?;
    // Windows has no exec bit; `%TEMP%` is already private to the user.
    #[cfg(unix)]
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
        .context("setting configured-harness startup script mode to 0700")?;
    // Close the writer before the pane executes this file (Linux rejects an
    // open-for-write executable with ETXTBSY). `keep` transfers cleanup to the
    // script after runner success, or back to the daemon after runner failure.
    let (script_file, script_path) = script
        .keep()
        .context("persisting configured-harness startup script")?;
    drop(script_file);

    let run_result = pane_run_argv(pane_id, &script_path).and_then(|runner_argv| {
        runner
            .run(socket, &runner_argv)
            .map_err(mark_retryable_runner_race)
            .map_err(|error| {
                let message = format!("{error:#}");
                error.context(format!("herdr pane run {pane_id}: {message}"))
            })
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

/// The `herdr` argv that types the startup-script invocation into `pane_id`.
/// Always exactly four items; the last is the whole line the pane shell runs.
#[cfg(unix)]
fn pane_run_argv(pane_id: &str, script_path: &Path) -> anyhow::Result<Vec<String>> {
    Ok(vec![
        "pane".to_string(),
        "run".to_string(),
        pane_id.to_string(),
        script_path.to_string_lossy().into_owned(),
    ])
}

#[cfg(windows)]
fn pane_run_argv(pane_id: &str, script_path: &Path) -> anyhow::Result<Vec<String>> {
    let host = if board_core::exec::resolve_program("pwsh").is_some() {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    Ok(vec![
        "pane".to_string(),
        "run".to_string(),
        pane_id.to_string(),
        windows_pane_command(host, &script_path.to_string_lossy())?,
    ])
}

/// The argv the startup script runs; the sh script takes it verbatim.
#[cfg(unix)]
fn script_argv(argv: &[String]) -> anyhow::Result<Vec<String>> {
    Ok(argv.to_vec())
}

/// On Windows the program is resolved here, through the same PATH/PATHEXT
/// lookup every other board spawn uses, so the script invokes an absolute
/// path: `& 'name'` would otherwise prefer a PowerShell alias, function or
/// cmdlet of that name (`curl`, `where`, …) over the application. An
/// unresolvable name stays bare and fails in the pane as "not found" (127).
/// A batch-file program (npm's `.cmd` shims) is refused up front when an
/// argument would not reach it intact (see [`pwsh_batch_argv_problem`]).
#[cfg(windows)]
fn script_argv(argv: &[String]) -> anyhow::Result<Vec<String>> {
    let mut resolved = argv.to_vec();
    if let Some(program) = resolved.first_mut() {
        if let Some(path) = board_core::exec::resolve_program(program)
            .and_then(|path| path.into_os_string().into_string().ok())
        {
            *program = path;
        }
    }
    if let Some((program, args)) = resolved.split_first() {
        if let Some(problem) = pwsh_batch_argv_problem(Path::new(program), args) {
            bail!("cannot launch configured harness {}: {problem}", argv[0]);
        }
    }
    Ok(resolved)
}

#[cfg(unix)]
fn startup_script(path: &Path, argv: &[String]) -> String {
    configured_script(path, argv)
}

#[cfg(windows)]
fn startup_script(path: &Path, argv: &[String]) -> String {
    configured_script_ps1(path, argv)
}

/// Why `args` cannot be handed to the batch file `program` from the `.ps1`
/// startup script, or `None` when they can (or `program` is not a batch
/// file). PowerShell passes `.cmd`/`.bat` targets their arguments with
/// legacy quoting even in pwsh 7.3+ (`$PSNativeCommandArgumentPassing =
/// 'Windows'`), which drops empty arguments and does not escape embedded
/// double quotes, and cmd.exe cuts an argument at a line break. Those are
/// refused rather than silently mangled or truncated.
#[cfg(any(windows, test))]
pub(crate) fn pwsh_batch_argv_problem(program: &Path, args: &[String]) -> Option<String> {
    let is_batch = program
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return None;
    }
    let (index, reason) = args.iter().enumerate().find_map(|(index, arg)| {
        let reason = if arg.contains(['\r', '\n']) {
            "contains a line break"
        } else if arg.contains('"') {
            "contains a double quote"
        } else if arg.is_empty() {
            "is empty"
        } else {
            return None;
        };
        Some((index, reason))
    })?;
    Some(format!(
        "{} is a batch file and argument {} {reason}, which PowerShell cannot pass to a \
         .cmd/.bat program intact; point the harness at the real executable instead of its shim",
        program.display(),
        index + 1
    ))
}

#[cfg(any(unix, test))]
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

#[cfg(any(unix, test))]
pub(crate) fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The PowerShell counterpart of [`configured_script`], with the same
/// semantics line for line:
///
/// - remove itself first, ignoring failure (`rm -f`);
/// - run the exact argv with `&`, every element a single-quoted literal;
/// - the child status is `$LASTEXITCODE`; a program that cannot be found is
///   127 and any other failure to run it is 1 (sh's 127/126), and the
///   script carries on either way;
/// - when `BOARD_BIN` is non-empty, call `__pane-exited --run-id
///   "$BOARD_RUN_ID"` and ignore its failure (`|| :`);
/// - exit with the child status.
///
/// The leading BOM makes Windows PowerShell 5.1 read the file as UTF-8
/// rather than the ANSI code page, so non-ASCII argv survives on both hosts.
/// Argument boundaries are exact for native executables under pwsh 7.3+;
/// older hosts' legacy native-argument passing can drop empty arguments and
/// mangle embedded double quotes, and every host passes batch files that
/// way (so `script_argv` refuses such arguments for a `.cmd`/`.bat`).
#[cfg(any(windows, test))]
pub(crate) fn configured_script_ps1(path: &Path, argv: &[String]) -> String {
    let mut script = String::from(
        "\u{feff}# herdr-board configured-harness startup script (generated; removes itself).\n",
    );
    script.push_str("Remove-Item -LiteralPath ");
    script.push_str(&powershell_quote(&path.to_string_lossy()));
    script.push_str(" -Force -ErrorAction SilentlyContinue\n");
    script.push_str("$global:LASTEXITCODE = 0\n$childStatus = 0\ntry {\n\t&");
    for arg in argv {
        script.push(' ');
        script.push_str(&powershell_quote(arg));
    }
    script.push_str(
        "\n\t$childStatus = $global:LASTEXITCODE\n\
         \tif ($null -eq $childStatus) { $childStatus = 0 }\n\
         } catch [System.Management.Automation.CommandNotFoundException] {\n\
         \t[Console]::Error.WriteLine($_.Exception.Message)\n\
         \t$childStatus = 127\n\
         } catch {\n\
         \t[Console]::Error.WriteLine($_.Exception.Message)\n\
         \t$childStatus = 1\n\
         }\n\
         if ($env:BOARD_BIN) {\n\
         \ttry { & $env:BOARD_BIN '__pane-exited' '--run-id' \"$env:BOARD_RUN_ID\" } catch { }\n\
         }\n\
         exit $childStatus\n",
    );
    script
}

/// Quote `value` as a PowerShell verbatim (single-quoted) string. PowerShell
/// treats the typographic quotes U+2018..U+201B as single quotes too, so each
/// of them is doubled as well; two adjacent quote characters read back as
/// one. Nothing else (`$`, backtick, newlines) is special inside the quotes.
#[cfg(any(windows, test))]
pub(crate) fn powershell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(ch);
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

/// Characters a double-quoted path cannot carry through every pane shell:
/// `"` ends the quotes, `$`/backtick expand in PowerShell, `%` expands in
/// cmd, and `!` is history expansion in an interactive sh.
#[cfg(any(windows, test))]
const PANE_COMMAND_UNSAFE: &[char] = &['"', '$', '`', '%', '!'];

/// The one line typed into a Windows pane to run the startup script. `host`
/// is the bare PowerShell executable name, which every pane shell resolves
/// through `PATH`; the script path is double-quoted, the one quoting form
/// pwsh, Windows PowerShell, cmd and sh all agree on. A path that cannot be
/// quoted that way is refused instead of being typed half-escaped.
#[cfg(any(windows, test))]
pub(crate) fn windows_pane_command(host: &str, script_path: &str) -> anyhow::Result<String> {
    if let Some(bad) = script_path
        .chars()
        .find(|ch| ch.is_control() || PANE_COMMAND_UNSAFE.contains(ch))
    {
        bail!(
            "configured-harness startup script path {script_path:?} contains {bad:?}, which \
             cannot be quoted for every pane shell; point TEMP at a directory without it"
        );
    }
    Ok(format!(
        "{host} -NoProfile -ExecutionPolicy Bypass -File \"{script_path}\""
    ))
}

pub(crate) fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
