//! Project subcommands.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    /// List all projects (folder names; Global last).
    List,
    /// Show one project by path, or the selected project.
    Show { path: Option<String> },
    /// Create a project for an existing directory (never creates directories).
    Create { path: String },
    /// Select a project; optionally pick one of its boards by id or name.
    Select {
        path: String,
        /// Board id or name within the project.
        #[arg(long, value_name = "ID|NAME")]
        board: Option<String>,
    },
}
