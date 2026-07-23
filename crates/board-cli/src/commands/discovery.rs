use anyhow::{bail, Result};
use board_core::client::BoardClient;
use serde_json::json;

use crate::args::{HarnessCmd, SessionCmd, SpaceCmd};
use crate::daemon::connect_or_start;
use crate::helpers::{efforts_str, harness_capabilities, print_json, union_efforts};

pub(crate) fn cmd_status(json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let s = c.daemon_status()?;
    if json {
        print_json(&s)?;
    } else {
        println!(
            "boardd {}  db={}  herdr={}  active={}  queued={}",
            s.version,
            s.db_path,
            if s.herdr_connected {
                "connected"
            } else {
                "absent"
            },
            s.active_runs,
            s.queued_runs
        );
    }
    Ok(())
}

pub(crate) fn cmd_harness(sub: HarnessCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        HarnessCmd::List { json } => {
            let names = c.harness_list()?.harnesses;
            if json {
                print_json(&names)?;
            } else {
                for h in &names {
                    println!("{h}");
                }
            }
        }
        HarnessCmd::Models { harness, json } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            if json {
                print_json(&caps)?;
            } else {
                for m in &caps.models {
                    println!("{}  {}", m.id, efforts_str(&m.efforts));
                }
                if caps.model_freeform {
                    if caps.models.is_empty() {
                        println!("(any model string accepted; catalog comes from harness config)");
                    } else {
                        println!("\n(any model string accepted; these are known aliases)");
                    }
                }
            }
        }
        HarnessCmd::Efforts {
            harness,
            model,
            json,
        } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            let (efforts, known) = match caps.models.iter().find(|m| m.id == model) {
                Some(m) => (m.efforts.clone(), true),
                None if caps.model_freeform => (union_efforts(&caps), false),
                None => bail!("model '{model}' not known to harness '{harness}'"),
            };
            if json {
                let efforts: Vec<&str> = efforts.iter().map(|e| e.as_str()).collect();
                print_json(&json!({ "model": model, "efforts": efforts, "known": known }))?;
            } else {
                println!("{}", efforts_str(&efforts));
                if !known {
                    println!(
                        "\n(model '{model}' unknown to {harness} but accepted; \
                         showing all known efforts)"
                    );
                }
            }
        }
        HarnessCmd::Permissions { harness, json } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            if json {
                print_json(&caps.permission_modes)?;
            } else {
                for p in &caps.permission_modes {
                    println!("{p}");
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn cmd_space(sub: SpaceCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        SpaceCmd::List { session, json } => {
            let res = c.space_list(session.as_deref())?;
            if json {
                print_json(&res)?;
            } else {
                let width = res.spaces.iter().map(|s| s.id.len()).max().unwrap_or(0);
                for s in &res.spaces {
                    println!("{:<width$}  {}", s.id, s.label);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn cmd_session(sub: SessionCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        SessionCmd::List { json } => {
            let res = c.session_list()?;
            if json {
                print_json(&res)?;
            } else {
                let width = res.sessions.iter().map(|s| s.name.len()).max().unwrap_or(0);
                for s in &res.sessions {
                    let running = if s.running { "running" } else { "stopped" };
                    let marker = if s.default { "  (default)" } else { "" };
                    println!("{:<width$}  {:<8}{}", s.name, running, marker);
                }
            }
        }
    }
    Ok(())
}
