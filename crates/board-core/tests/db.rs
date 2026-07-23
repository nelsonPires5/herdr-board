//! Db migrations, seed, CRUD, and position management.

#[path = "db/crud.rs"]
mod crud;
#[path = "db/migrations.rs"]
mod migrations;
#[path = "db/runs.rs"]
mod runs;

use board_core::db::Db;

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}
