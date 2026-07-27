//! Read-only discovery subcommands: harness capabilities, spaces, sessions.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum HarnessCmd {
    /// List every available harness.
    List,
    /// List known models and the efforts each accepts.
    Models {
        #[arg(default_value = "pi")]
        harness: String,
    },
    /// Show the efforts a model accepts.
    Efforts {
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        model: String,
    },
    /// List permission modes a harness understands.
    Permissions {
        #[arg(default_value = "pi")]
        harness: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SpaceCmd {
    /// List run spaces (herdr workspaces) in a session.
    List {
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCmd {
    /// List herdr sessions.
    List,
}
