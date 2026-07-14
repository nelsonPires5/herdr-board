//! Manual smoke test: run the full kanban TUI against a seeded in-memory
//! FakeBoardClient. No daemon, no herdr.
//!
//! `cargo run -p board-tui --example tui_fake --features fake-client`

fn main() -> anyhow::Result<()> {
    let client = board_tui::testkit::demo_client()?;
    board_tui::run(Box::new(client))
}
