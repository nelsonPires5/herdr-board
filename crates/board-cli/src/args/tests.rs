//! Parse-level contract tests. These run in milliseconds and need no daemon,
//! so every alias, hidden flag, and conflict is pinned here rather than through
//! a spawned boardd.

use super::*;
use clap::CommandFactory;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|error| panic!("{args:?} should parse: {error}"))
}

fn reject(args: &[&str]) -> clap::Error {
    match Cli::try_parse_from(args) {
        Ok(_) => panic!("{args:?} unexpectedly parsed"),
        Err(error) => error,
    }
}

#[test]
fn the_command_tree_is_internally_consistent() {
    Cli::command().debug_assert();
}

// -- global --json ------------------------------------------------------------

#[test]
fn json_is_accepted_before_or_after_the_subcommand_path() {
    assert!(parse(&["board", "card", "list", "--json"]).json);
    assert!(parse(&["board", "--json", "card", "list"]).json);
    assert!(parse(&["board", "card", "--json", "list"]).json);
    assert!(!parse(&["board", "card", "list"]).json);
}

/// A2: a positional whose *value* happens to be `--json` must not turn on JSON
/// rendering. Only parsing decides.
#[test]
fn a_json_shaped_argument_value_does_not_enable_json() {
    let cli = parse(&["board", "card", "comment", "add", "7", "--", "--json"]);
    assert!(!cli.json);
    match cli.cmd {
        Cmd::Card {
            sub:
                CardCmd::Comment {
                    sub: CommentCmd::Add { card_id, body },
                },
        } => {
            assert_eq!(card_id, 7);
            assert_eq!(body, "--json");
        }
        _ => panic!("expected card comment add"),
    }
}

#[test]
fn the_global_board_selector_is_accepted_at_any_depth() {
    assert_eq!(
        parse(&["board", "--board", "12", "card", "list"])
            .board
            .as_deref(),
        Some("12")
    );
    assert_eq!(
        parse(&["board", "card", "list", "--board", "12"])
            .board
            .as_deref(),
        Some("12")
    );
}

// -- daemon taxonomy (D1) -----------------------------------------------------

#[test]
fn daemon_gains_start_stop_and_status_subcommands() {
    assert!(matches!(
        parse(&["board", "daemon", "start"]).cmd,
        Cmd::Daemon {
            sub: Some(DaemonCmd::Start { foreground: false }),
            ..
        }
    ));
    assert!(matches!(
        parse(&["board", "daemon", "start", "--foreground"]).cmd,
        Cmd::Daemon {
            sub: Some(DaemonCmd::Start { foreground: true }),
            ..
        }
    ));
    assert!(matches!(
        parse(&["board", "daemon", "stop"]).cmd,
        Cmd::Daemon {
            sub: Some(DaemonCmd::Stop),
            ..
        }
    ));
    assert!(matches!(
        parse(&["board", "daemon", "status"]).cmd,
        Cmd::Daemon {
            sub: Some(DaemonCmd::Status),
            ..
        }
    ));
}

#[test]
fn bare_daemon_keeps_its_flag_grammar() {
    assert!(matches!(
        parse(&["board", "daemon"]).cmd,
        Cmd::Daemon {
            foreground: false,
            stop: false,
            sub: None
        }
    ));
    assert!(matches!(
        parse(&["board", "daemon", "--foreground"]).cmd,
        Cmd::Daemon {
            foreground: true,
            sub: None,
            ..
        }
    ));
    assert!(matches!(
        parse(&["board", "daemon", "--stop"]).cmd,
        Cmd::Daemon {
            stop: true,
            sub: None,
            ..
        }
    ));
}

#[test]
fn daemon_status_still_takes_json() {
    let cli = parse(&["board", "daemon", "status", "--json"]);
    assert!(cli.json);
    assert!(matches!(
        cli.cmd,
        Cmd::Daemon {
            sub: Some(DaemonCmd::Status),
            ..
        }
    ));
}

// -- conflicts (B6) -----------------------------------------------------------

#[test]
fn fresh_and_reuse_session_conflict_at_parse_time() {
    for command in [
        vec![
            "board",
            "column",
            "create",
            "--name",
            "x",
            "--fresh-session",
            "--reuse-session",
        ],
        vec![
            "board",
            "column",
            "edit",
            "1",
            "--fresh-session",
            "--reuse-session",
        ],
    ] {
        let error = reject(&command);
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
    // Either flag on its own stays valid.
    parse(&[
        "board",
        "column",
        "create",
        "--name",
        "x",
        "--fresh-session",
    ]);
    parse(&["board", "column", "edit", "1", "--reuse-session"]);
}

// -- retained spellings -------------------------------------------------------

#[test]
fn legacy_top_level_verbs_still_parse() {
    assert!(matches!(
        parse(&["board", "comment", "body only"]).cmd,
        Cmd::Comment { body: None, .. }
    ));
    assert!(matches!(
        parse(&["board", "comment", "5", "a body"]).cmd,
        Cmd::Comment { body: Some(_), .. }
    ));
    assert!(matches!(
        parse(&["board", "done", "5", "--outcome", "ok"]).cmd,
        Cmd::Done { .. }
    ));
    assert!(matches!(
        parse(&["board", "cancel", "5"]).cmd,
        Cmd::Cancel { card_id: 5 }
    ));
    assert!(matches!(
        parse(&["board", "retry", "5"]).cmd,
        Cmd::Retry { card_id: 5 }
    ));
    assert!(matches!(
        parse(&["board", "move", "5", "Done"]).cmd,
        Cmd::Move { card_id: 5, .. }
    ));
}

#[test]
fn card_new_and_to_board_remain_aliases() {
    assert!(matches!(
        parse(&["board", "card", "new", "--title", "t"]).cmd,
        Cmd::Card {
            sub: CardCmd::Create { .. }
        }
    ));
    match parse(&["board", "card", "move", "5", "Done", "--to-board", "9"]).cmd {
        Cmd::Card {
            sub: CardCmd::Move {
                destination_board, ..
            },
        } => assert_eq!(destination_board.as_deref(), Some("9")),
        _ => panic!("expected card move"),
    }
}

/// D3: `--destination-board` is the explicit cross-board spelling on both the
/// nested and the legacy verb.
#[test]
fn move_accepts_an_explicit_destination_board() {
    match parse(&["board", "move", "5", "Done", "--destination-board", "9"]).cmd {
        Cmd::Move {
            destination_board, ..
        } => assert_eq!(destination_board.as_deref(), Some("9")),
        _ => panic!("expected move"),
    }
}

#[test]
fn destructive_commands_share_one_confirmation_flag() {
    assert!(matches!(
        parse(&["board", "card", "delete", "1", "--yes"]).cmd,
        Cmd::Card {
            sub: CardCmd::Delete {
                confirm: ConfirmArgs { yes: true },
                ..
            }
        }
    ));
    assert!(matches!(
        parse(&["board", "column", "delete", "Todo", "--yes"]).cmd,
        Cmd::Column {
            sub: ColumnCmd::Delete {
                confirm: ConfirmArgs { yes: true },
                ..
            }
        }
    ));
    assert!(matches!(
        parse(&["board", "card", "comment", "delete", "1", "--yes"]).cmd,
        Cmd::Card {
            sub: CardCmd::Comment {
                sub: CommentCmd::Delete {
                    confirm: ConfirmArgs { yes: true },
                    ..
                }
            }
        }
    ));
}

#[test]
fn top_level_status_is_not_a_command() {
    assert_eq!(
        reject(&["board", "status"]).kind(),
        clap::error::ErrorKind::InvalidSubcommand
    );
}
