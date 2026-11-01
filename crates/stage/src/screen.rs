//! Presentation for full-screen Stage. Keep our own back buffer so a native
//! terminal scroll can move existing rows, then diff only the exposed/changed
//! cells. Ratatui's regular diff cannot know that the terminal already scrolled.
//! No graphics or sidebars are ever moved by the fast path. Rex supports
//! horizontal margins, so a text feed beside the sidebar can use it too.

use std::io::{self, Write};
use ratatui::{backend::{Backend, CrosstermBackend}, buffer::{Buffer, Cell, CellDiffOption}, layout::Rect, Terminal, TerminalOptions, Viewport};
use crate::{app::App, ui};

pub struct Screen {
    previous: Buffer,
    next: Buffer,
    shifted: Buffer,
    scroll: usize,
    scroll_region: Option<Rect>,
    horizontal_margins: bool,
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
            scroll_region: None,
            horizontal_margins: std::env::var("TERM_PROGRAM").is_ok_and(|name| name == "rex"),
            enabled: std::env::var_os("STAGE_NO_SCROLL_REGION").is_none(),
            invalidated: false,
        }
    }
}

/// Shift full rows inside a scroll region, clearing the newly exposed rows.
fn shift(buffer: &mut Buffer, region: Rect, delta: isize) {
    let (top, bottom) = (region.y, region.bottom());
    if region.width != buffer.area.width {
        // Copy in movement order so the source row hasn't been overwritten.
        for offset in 0..region.height {
            let y = if delta > 0 { top + offset } else { bottom - 1 - offset };
            let source = y as isize + delta;
            for x in region.x..region.right() {
                if (top as isize..bottom as isize).contains(&source) {
                    buffer[(x, y)] = buffer[(x, source as u16)].clone();
                } else {
                    buffer[(x, y)].reset();
                }
            }
        }
        return;
    }
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
    /// whether every cell was repainted, so graphics must be placed again.
    pub fn draw<W: Write + Default>(&mut self, term: &mut Terminal<CrosstermBackend<W>>, app: &mut App) -> io::Result<bool> {
        // Never clear the screen: Rex deletes every kitty image on a clear but keeps
        // them across a resize, and Ratatui's own resize clears. Size a fresh
        // Terminal instead (after flushing the old one's writer) and repaint every
        // cell over whatever the terminal kept.
        let area = Rect::new(0, 0, app.size.0, app.size.1);
        if term.current_buffer_mut().area != area {
            Backend::flush(term.backend_mut())?;
            *term = Terminal::with_options(CrosstermBackend::new(W::default()), TerminalOptions { viewport: Viewport::Fixed(area) })?;
        }
        let resized = self.previous.area != area;
        let repaint = resized || self.invalidated;
        if resized {
            self.next = Buffer::empty(area);
            self.shifted = Buffer::empty(area);
        } else if repaint {
            self.next.reset();
            self.shifted.reset();
        }
        if repaint {
            // Unequal to every drawn cell, so the diff rewrites the whole screen.
            let mut stale = Cell::default();
            stale.set_diff_option(CellDiffOption::Skip);
            self.previous = Buffer::filled(area, stale);
        }
        {
            let mut frame = term.get_frame();
            ui::draw(&mut frame, app);
        }
        // Exchange reusable buffers instead of copying a full screen per frame.
        std::mem::swap(term.current_buffer_mut(), &mut self.next);
        let g = ui::geometry_for(app);
        let region = Rect::new(g.main.x, g.main.y + 1, g.main.width, g.main.height.saturating_sub(1));
        let movable = (region.width == area.width || self.horizontal_margins)
            && region.intersection(area) == region && !region.is_empty()
            && ui::session_area(app).is_none() && app.on_screen.is_empty();
        let delta = app.scroll as isize - self.scroll as isize;
        let mut scrolled = false;
        if self.enabled && !repaint && self.scroll_region == Some(region) && movable
            && delta != 0 && delta.unsigned_abs() < region.height as usize {
            self.shifted.content.clone_from(&self.previous.content);
            shift(&mut self.shifted, region, delta);
            let plain = self.previous.diff_iter(&self.next).count();
            let shifted = self.shifted.diff_iter(&self.next).count();
            // A scroll command has overhead. Use it only when it saves at least
            // one row of cells; arbitrary content changes still diff correctly.
            if shifted + (region.width as usize) < plain {
                let direction = if delta > 0 { 'S' } else { 'T' };
                let partial_width = region.width != area.width;
                if partial_width {
                    write!(term.backend_mut(), "\x1b[?69h\x1b[{};{}s", region.x + 1, region.right())?;
                }
                write!(term.backend_mut(), "\x1b[0m\x1b[{};{}r\x1b[{}{}\x1b[r", region.y + 1, region.bottom(), delta.unsigned_abs(), direction)?;
                if partial_width {
                    term.backend_mut().write_all(b"\x1b[s\x1b[?69l")?;
                }
                term.backend_mut().draw(self.shifted.diff_iter(&self.next))?;
                scrolled = true;
            }
        }
        if !scrolled { term.backend_mut().draw(self.previous.diff_iter(&self.next))?; }
        std::mem::swap(&mut self.previous, &mut self.next);
        self.next.reset();
        self.scroll = app.scroll;
        self.scroll_region = movable.then_some(region);
        self.invalidated = false;
        Ok(repaint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_resize_repaints_without_clearing_then_returns_to_diffing() {
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
        assert!(screen.draw(&mut term, &mut app).unwrap(), "graphics must be placed again too");
        let repaint = take_output();
        assert!(!repaint.windows(4).any(|w| w == b"\x1b[2J" || w.starts_with(b"\x1b[J")), "a clear would delete Rex's images");
        assert_eq!(repaint, first, "every cell is rewritten, as on the first frame");

        assert!(!screen.draw(&mut term, &mut app).unwrap());
        assert_eq!(take_output(), unchanged);
    }

    #[test]
    fn region_scroll_preserves_bars_and_clears_exposed_rows() {
        let original = Buffer::with_lines(["header", "title ", "row 01", "row 02", "row 03", "footer"]);
        let mut up = original.clone();
        shift(&mut up, Rect::new(0, 2, 6, 3), 1);
        assert_eq!(up, Buffer::with_lines(["header", "title ", "row 02", "row 03", "      ", "footer"]));
        let mut down = original;
        shift(&mut down, Rect::new(0, 2, 6, 3), -2);
        assert_eq!(down, Buffer::with_lines(["header", "title ", "      ", "      ", "row 01", "footer"]));
    }

    #[test]
    fn rectangular_scroll_preserves_sidebar_title_and_footer() {
        let original = Buffer::with_lines(["HEADER", "A:1111", "B:2222", "C:3333", "FOOTER"]);
        let region = Rect::new(2, 1, 4, 3);
        let mut up = original.clone();
        shift(&mut up, region, 1);
        assert_eq!(up, Buffer::with_lines(["HEADER", "A:2222", "B:3333", "C:    ", "FOOTER"]));
        let mut down = original;
        shift(&mut down, region, -1);
        assert_eq!(down, Buffer::with_lines(["HEADER", "A:    ", "B:1111", "C:2222", "FOOTER"]));
    }

    #[test]
    fn shifted_diff_reconstructs_all_cells_including_style_changes() {
        let mut before = Buffer::with_lines(["head", "aaaa", "bbbb", "cccc", "dddd", "foot"]);
        before[(1, 2)].set_fg(ratatui::style::Color::Blue);
        let after = Buffer::with_lines(["HEAD", "bbbb", "cccc", "dddd", "eeee", "FOOT"]);
        shift(&mut before, Rect::new(0, 1, 4, 4), 1);
        let changes = before.diff(&after).into_iter().map(|(x,y,c)| (x,y,c.clone())).collect::<Vec<_>>();
        for (x,y,c) in changes { before[(x,y)] = c; }
        assert_eq!(before, after);
    }
}
