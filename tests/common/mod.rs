// Keep shared integration-test helpers under `common/mod.rs` so Cargo does not
// treat the helper module itself as a separate zero-test integration target.

use color_eyre::eyre::Result;
use ratatui::{Terminal, backend::TestBackend};

pub fn test_terminal() -> Result<Terminal<TestBackend>> {
    let backend = TestBackend::new(80, 24);
    Ok(Terminal::new(backend)?)
}
