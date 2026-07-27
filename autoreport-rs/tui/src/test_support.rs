//! Test-only helpers shared across TUI tests.

use ratatui::backend::{Backend, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// `custom_terminal::Terminal` requires `B: Backend + std::io::Write`, but
/// ratatui's `TestBackend` is not `Write`. This newtype wraps `TestBackend`
/// with a no-op `Write` so render tests can drive the custom Terminal through
/// `terminal.draw(|f| screen.draw(f))` exactly like production, then assert
/// on the rendered frame via [`custom_terminal::Terminal::rendered_buffer`].
pub(crate) struct WritableTestBackend {
    inner: TestBackend,
}

impl WritableTestBackend {
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
        }
    }
}

impl std::io::Write for WritableTestBackend {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // The custom terminal flushes a byte stream of cursor/style/print
        // commands; tests assert on the cell buffer instead, so discard bytes.
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Backend for WritableTestBackend {
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }

    fn size(&self) -> std::io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
