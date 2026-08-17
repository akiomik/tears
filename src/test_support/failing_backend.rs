//! A backend whose render fails, for the one termination cause no
//! application can produce.
//!
//! Three of the four termination causes are reachable from application code:
//! an `update`-returned quit, a producer-originated quit, and a host-side
//! drop. The fourth — a render failure — belongs to the backend, so driving
//! it needs a backend that fails rather than a hook inside the kernel. That
//! is the whole reason this type exists: it keeps `Program::view` and the
//! terminal as the only render owners (RFC 0011 INV-LC5, RFC 0014 §7.2's
//! third excluded driving difference).

use std::io;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// A backend that draws nothing and fails its draw once its healthy draws
/// are spent.
///
/// Everything but `draw` succeeds, so a `Terminal` is constructible over it
/// and the failure lands exactly at the pass stage that renders.
#[derive(Clone, Copy, Debug)]
pub struct FailingBackend {
    size: Size,
    healthy_draws: usize,
}

impl FailingBackend {
    /// A backend of the given size whose first `healthy_draws` draws succeed
    /// and whose next one fails.
    ///
    /// Zero makes the very first render fail, which is the bootstrap
    /// continuation pass's; one lets bootstrap through and fails the first
    /// steady-state pass that renders.
    pub const fn new(width: u16, height: u16, healthy_draws: usize) -> Self {
        Self {
            size: Size { width, height },
            healthy_draws,
        }
    }
}

impl Backend for FailingBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, _content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let Some(remaining) = self.healthy_draws.checked_sub(1) else {
            return Err(io::Error::other("the failing backend refuses to draw"));
        };
        self.healthy_draws = remaining;
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, _position: P) -> io::Result<()> {
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size {
                width: 0,
                height: 0,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
