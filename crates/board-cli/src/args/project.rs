//! Project subcommands.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    /// List all projects (folder names; Global last).
    List {
        /// Filter by archive state: active (default), all, or archived.
        #[arg(long, value_parser = ["active", "all", "archived"])]
        visibility: Option<String>,
    },
    /// Show one project by path, or the selected project.
    Show {
        path: Option<String>,
        /// Filter boards inside the project by archive state.
        #[arg(long, value_parser = ["active", "all", "archived"])]
        visibility: Option<String>,
    },
    /// Create a project for an existing directory (never creates directories).
    Create { path: String },
    /// Select a project; optionally pick one of its boards by id or name.
    Select {
        path: String,
        /// Board id or name within the project.
        #[arg(long, value_name = "ID|NAME")]
        board: Option<String>,
    },
    /// Archive a project (all boards must already be archived; Global never).
    Archive { path: String },
    /// Restore an archived project.
    Restore { path: String },
}
