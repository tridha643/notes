//! Presentation for full-screen Stage. Keep our own back buffer so a native
//! terminal scroll can move existing rows, then diff only the exposed/changed
//! cells. Ratatui's regular diff cannot know that the terminal already scrolled.
//! No graphics or sidebars are ever moved by the fast path.

use std::io::{self, Write};
use ratatui::{backend::{Backend, CrosstermBackend}, buffer::Buffer, layout::Rect, Terminal};
use crate::{app::App, ui};

pub struct Screen {
    previous: Buffer,
    next: Buffer,
    shifted: Buffer,
    scroll: usize,
    movable: bool,
    enabled: bool,
    invalidated: bool,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            previous: Buffer::empty(Rect::default()),
            next: Buffer::empty(Rect::default()),
            shifted: Buffer::empty(Rect::default()),
            scroll: 0,
            movable: false,
            enabled: std::env::var_os("STAGE_NO_SCROLL_REGION").is_none(),
            invalidated: false,
        }
    }
}

/// Shift full rows inside a scroll region, clearing the newly exposed rows.
fn shift(buffer: &mut Buffer, top: u16, bottom: u16, delta: isize) {
    let width = buffer.area.width as usize;
    let start = top as usize * width;
    let end = bottom as usize * width;
    let count = delta.unsigned_abs() * width;
    let rows = &mut buffer.content[start..end];
    if delta > 0 {
        rows.rotate_left(count);
        let clear = rows.len() - count;
        for cell in &mut rows[clear..] { cell.reset(); }
    } else {
        rows.rotate_right(count);
        for cell in &mut rows[..count] { cell.reset(); }
    }
}

impl Screen {
    /// A resize notification invalidates terminal contents even if the final size
    /// is unchanged: coalesced SIGWINCH signals can hide a shrink/grow round trip.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// Uses Ratatui's documented manual-rendering API. We own presentation and
    /// cursor policy (Stage keeps the terminal cursor hidden), so never mix this
    /// with Terminal::draw/flush. The caller flushes once after graphics and the
    /// synchronized-output end marker, making one buffered frame write. Returns
    /// whether graphics must be restored after a screen clear.
    pub fn draw<W: Write>(&mut self, term: &mut Terminal<CrosstermBackend<W>>, app: &mut App) -> io::Result<bool> {
        term.autoresize()?;
        let area = term.current_buffer_mut().area;
        let resized = self.previous.area != area;
        let cleared = resized || self.invalidated;
        if self.invalidated && !resized {
            // We own the full-screen buffers and keep the cursor hidden. Avoid
            // Terminal::clear's cursor query, which would race the input reader.
            term.backend_mut().clear()?;
        }
        if cleared {
            self.previous = Buffer::empty(area);
            self.next = Buffer::empty(area);
            self.shifted = Buffer::empty(area);
        }
        {
            let mut frame = term.get_frame();
            ui::draw(&mut frame, app);
        }
        // Exchange reusable buffers instead of copying a full screen per frame.
        std::mem::swap(term.current_buffer_mut(), &mut self.next);
        let g = ui::geometry_for(app);
        let movable = g.main.x == 0 && g.main.width == area.width
            && ui::session_area(app).is_none() && app.on_screen.is_empty();
        let delta = app.scroll as isize - self.scroll as isize;
        let (top, bottom) = (g.main.y + 1, g.main.bottom());
        let mut scrolled = false;
        if self.enabled && !cleared && self.movable && movable
            && delta != 0 && delta.unsigned_abs() < bottom.saturating_sub(top) as usize {
            self.shifted.content.clone_from(&self.previous.content);
            shift(&mut self.shifted, top, bottom, delta);
            let plain = self.previous.diff_iter(&self.next).count();
            let shifted = self.shifted.diff_iter(&self.next).count();
            // A scroll command has overhead. Use it only when it saves at least
            // one row of cells; arbitrary content changes still diff correctly.
            if shifted + (area.width as usize) < plain {
                let direction = if delta > 0 { 'S' } else { 'T' };
                write!(term.backend_mut(), "\x1b[0m\x1b[{};{}r\x1b[{}{}\x1b[r", top + 1, bottom, delta.unsigned_abs(), direction)?;
                term.backend_mut().draw(self.shifted.diff_iter(&self.next))?;
                scrolled = true;
            }
        }
        if !scrolled { term.backend_mut().draw(self.previous.diff_iter(&self.next))?; }
        std::mem::swap(&mut self.previous, &mut self.next);
        self.next.reset();
        self.scroll = app.scroll;
        self.movable = movable;
        self.invalidated = false;
        Ok(cleared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_resize_clears_and_repaints_then_returns_to_diffing() {
        use ratatui::{TerminalOptions, Viewport};
        use std::{cell::RefCell, rc::Rc};

        #[derive(Clone, Default)]
        struct Output(Rc<RefCell<Vec<u8>>>);
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.0.borrow_mut().write(bytes) }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let output = Output::default();
        let take_output = || std::mem::take(&mut *output.0.borrow_mut());
        let mut app = App::new(None);
        app.size = (80, 24);
        let mut term = Terminal::with_options(
            CrosstermBackend::new(output.clone()),
            TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)) },
        ).unwrap();
        let mut screen = Screen::default();
        assert!(screen.draw(&mut term, &mut app).unwrap());
        let first = take_output();

        assert!(!screen.draw(&mut term, &mut app).unwrap());
        let unchanged = take_output();
        assert!(unchanged.len() < first.len());

        // Multiple SIGWINCH notifications may be coalesced into one frame whose
        // dimensions equal the previous frame. The terminal still lost cells.
        screen.invalidate();
        screen.invalidate();
        assert!(screen.draw(&mut term, &mut app).unwrap(), "graphics must be restored too");
        let repaint = take_output();
        assert!(repaint.starts_with(b"\x1b[2J"), "clear remnants before repainting");
        assert!(repaint.ends_with(&first), "every original cell must be repainted");

        assert!(!screen.draw(&mut term, &mut app).unwrap());
        assert_eq!(take_output(), unchanged);
    }

    #[test]
    fn region_scroll_preserves_bars_and_clears_exposed_rows() {
        let original = Buffer::with_lines(["header", "title ", "row 01", "row 02", "row 03", "footer"]);
        let mut up = original.clone();
        shift(&mut up, 2, 5, 1);
        assert_eq!(up, Buffer::with_lines(["header", "title ", "row 02", "row 03", "      ", "footer"]));
        let mut down = original;
        shift(&mut down, 2, 5, -2);
        assert_eq!(down, Buffer::with_lines(["header", "title ", "      ", "      ", "row 01", "footer"]));
    }

    #[test]
    fn shifted_diff_reconstructs_all_cells_including_style_changes() {
        let mut before = Buffer::with_lines(["head", "aaaa", "bbbb", "cccc", "dddd", "foot"]);
        before[(1, 2)].set_fg(ratatui::style::Color::Blue);
        let after = Buffer::with_lines(["HEAD", "bbbb", "cccc", "dddd", "eeee", "FOOT"]);
        shift(&mut before, 1, 5, 1);
        let changes = before.diff(&after).into_iter().map(|(x,y,c)| (x,y,c.clone())).collect::<Vec<_>>();
        for (x,y,c) in changes { before[(x,y)] = c; }
        assert_eq!(before, after);
    }
}
