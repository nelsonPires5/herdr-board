use anyhow::{bail, Result};
use board_core::client::BoardClient;
use serde_json::json;

use crate::args::{HarnessCmd, SessionCmd, SpaceCmd};
use crate::context::Ctx;
use crate::helpers::{efforts_str, harness_capabilities, union_efforts};
use crate::render::{emit, emit_line};

pub(crate) fn cmd_status(ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    let status = ctx.client()?.daemon_status()?;
    emit(&status, json)
}

pub(crate) fn cmd_harness(sub: HarnessCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        HarnessCmd::List => {
            let names = ctx.client()?.harness_list()?.harnesses;
            emit(&names, json)
        }
        HarnessCmd::Models { harness } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            emit(&caps, json)
        }
        HarnessCmd::Efforts { harness, model } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            let (efforts, known) = match caps.models.iter().find(|m| m.id == model) {
                Some(m) => (m.efforts.clone(), true),
                None if caps.model_freeform => (union_efforts(&caps), false),
                None => bail!("model '{model}' not known to harness '{harness}'"),
            };
            let mut text = efforts_str(&efforts);
            if !known {
                text.push_str(&format!(
                    "\n\n(model '{model}' unknown to {harness} but accepted; \
                     showing all known efforts)"
                ));
            }
            let efforts: Vec<&str> = efforts.iter().map(|e| e.as_str()).collect();
            emit_line(
                &json!({ "model": model, "efforts": efforts, "known": known }),
                json,
                text,
            )
        }
        HarnessCmd::Permissions { harness } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            emit(&caps.permission_modes, json)
        }
    }
}

pub(crate) fn cmd_space(sub: SpaceCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        SpaceCmd::List { session } => {
            let spaces = ctx.client()?.space_list(session.as_deref())?;
            emit(&spaces, json)
        }
    }
}

pub(crate) fn cmd_session(sub: SessionCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        SessionCmd::List => {
            let sessions = ctx.client()?.session_list()?;
            emit(&sessions, json)
        }
    }
}
