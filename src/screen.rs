//! A small VT100/xterm screen model used as a correctness oracle.
//!
//! The harness observes the bytes each product client writes to the outer pseudo-terminal. The
//! `stripped` stream in `pty.rs` is enough to notice that a marker was emitted, but not that the
//! final visible screen is correct. This model replays the byte stream into a character grid so
//! scenarios can assert what a user would actually see: the new marker is present, the previous
//! marker is gone, and the tail of an output burst is intact and in order.
//!
//! Only the sequences needed for those assertions are implemented (cursor movement, erase, scroll
//! regions, insert/delete, alternate screen, save/restore). Colors and attributes are ignored,
//! wide characters occupy one cell, and unsupported sequences are consumed without effect. It is a
//! deliberately conservative oracle, not a terminal emulator.

#[derive(Debug, Clone)]
struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    pending_wrap: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    saved_cursor: Option<(usize, usize)>,
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![' '; cols]; rows],
            row: 0,
            col: 0,
            pending_wrap: false,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            saved_cursor: None,
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        self.cells.resize(rows, vec![' '; cols]);
        for line in &mut self.cells {
            line.resize(cols, ' ');
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.row = self.row.min(rows.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    fn put(&mut self, ch: char) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }
        if self.pending_wrap {
            self.col = 0;
            self.line_feed();
            self.pending_wrap = false;
        }
        self.cells[self.row][self.col] = ch;
        if self.col + 1 >= self.cols {
            self.pending_wrap = true;
        } else {
            self.col += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        for _ in 0..count.min(bottom - top + 1) {
            self.cells[top..=bottom].rotate_left(1);
            self.cells[bottom] = vec![' '; self.cols];
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        for _ in 0..count.min(bottom - top + 1) {
            self.cells[top..=bottom].rotate_right(1);
            self.cells[top] = vec![' '; self.cols];
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.rows.saturating_sub(1));
        self.col = col.min(self.cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.row + 1..self.rows {
                    self.cells[row] = vec![' '; self.cols];
                }
            }
            1 => {
                self.erase_line(1);
                for row in 0..self.row {
                    self.cells[row] = vec![' '; self.cols];
                }
            }
            _ => {
                for line in &mut self.cells {
                    *line = vec![' '; self.cols];
                }
            }
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let line = &mut self.cells[self.row];
        let range = match mode {
            0 => self.col..self.cols,
            1 => 0..(self.col + 1).min(self.cols),
            _ => 0..self.cols,
        };
        for cell in &mut line[range] {
            *cell = ' ';
        }
    }

    fn insert_lines(&mut self, count: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bottom {
            return;
        }
        let bottom = self.scroll_bottom;
        for _ in 0..count.min(bottom - self.row + 1) {
            self.cells[self.row..=bottom].rotate_right(1);
            self.cells[self.row] = vec![' '; self.cols];
        }
    }

    fn delete_lines(&mut self, count: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bottom {
            return;
        }
        let bottom = self.scroll_bottom;
        for _ in 0..count.min(bottom - self.row + 1) {
            self.cells[self.row..=bottom].rotate_left(1);
            self.cells[bottom] = vec![' '; self.cols];
        }
    }

    fn insert_chars(&mut self, count: usize) {
        let line = &mut self.cells[self.row];
        for _ in 0..count.min(self.cols - self.col) {
            line[self.col..].rotate_right(1);
            line[self.col] = ' ';
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let line = &mut self.cells[self.row];
        for _ in 0..count.min(self.cols - self.col) {
            line[self.col..].rotate_left(1);
            line[self.cols - 1] = ' ';
        }
    }

    fn erase_chars(&mut self, count: usize) {
        let end = (self.col + count).min(self.cols);
        for cell in &mut self.cells[self.row][self.col..end] {
            *cell = ' ';
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    StringBody,
    StringEscape,
}

#[derive(Debug, Clone)]
pub struct Screen {
    primary: Grid,
    alternate: Grid,
    alternate_active: bool,
    state: State,
    params: String,
    private: bool,
    utf8: Vec<u8>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            primary: Grid::new(cols as usize, rows as usize),
            alternate: Grid::new(cols as usize, rows as usize),
            alternate_active: false,
            state: State::Ground,
            params: String::new(),
            private: false,
            utf8: Vec::new(),
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.primary.resize(cols as usize, rows as usize);
        self.alternate.resize(cols as usize, rows as usize);
    }

    pub fn size(&self) -> (u16, u16) {
        let grid = self.grid();
        (grid.cols as u16, grid.rows as u16)
    }

    fn grid(&self) -> &Grid {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.feed_byte(byte);
        }
    }

    fn feed_byte(&mut self, byte: u8) {
        match self.state {
            State::Ground => self.ground(byte),
            State::Escape => self.escape(byte),
            State::EscapeIntermediate => self.state = State::Ground,
            State::Csi => self.csi(byte),
            State::StringBody => {
                if byte == 0x07 {
                    self.state = State::Ground;
                } else if byte == 0x1b {
                    self.state = State::StringEscape;
                }
            }
            State::StringEscape => {
                self.state = if byte == b'\\' {
                    State::Ground
                } else if byte == 0x1b {
                    State::StringEscape
                } else {
                    State::StringBody
                };
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if !self.utf8.is_empty() {
            if (0x80..0xc0).contains(&byte) {
                self.utf8.push(byte);
                let needed = utf8_length(self.utf8[0]);
                if self.utf8.len() >= needed {
                    let ch = std::str::from_utf8(&self.utf8)
                        .ok()
                        .and_then(|text| text.chars().next())
                        .unwrap_or('\u{fffd}');
                    self.utf8.clear();
                    self.grid_mut().put(ch);
                }
                return;
            }
            self.utf8.clear();
            self.grid_mut().put('\u{fffd}');
        }
        match byte {
            0x1b => self.state = State::Escape,
            b'\n' | 0x0b | 0x0c => self.grid_mut().line_feed(),
            b'\r' => {
                let grid = self.grid_mut();
                grid.col = 0;
                grid.pending_wrap = false;
            }
            0x08 => {
                let grid = self.grid_mut();
                grid.col = grid.col.saturating_sub(1);
                grid.pending_wrap = false;
            }
            b'\t' => {
                let grid = self.grid_mut();
                let next = ((grid.col / 8) + 1) * 8;
                grid.col = next.min(grid.cols.saturating_sub(1));
                grid.pending_wrap = false;
            }
            0x00..=0x1f | 0x7f => {}
            0x80.. => {
                if utf8_length(byte) > 1 {
                    self.utf8.push(byte);
                } else {
                    self.grid_mut().put('\u{fffd}');
                }
            }
            _ => self.grid_mut().put(char::from(byte)),
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = State::Ground;
        match byte {
            b'[' => {
                self.state = State::Csi;
                self.params.clear();
                self.private = false;
            }
            b']' | b'P' | b'_' | b'^' | b'X' => self.state = State::StringBody,
            b'(' | b')' | b'*' | b'+' | b'#' | b'%' => self.state = State::EscapeIntermediate,
            b'7' => {
                let grid = self.grid_mut();
                grid.saved_cursor = Some((grid.row, grid.col));
            }
            b'8' => {
                let grid = self.grid_mut();
                if let Some((row, col)) = grid.saved_cursor {
                    grid.move_to(row, col);
                }
            }
            b'D' => self.grid_mut().line_feed(),
            b'E' => {
                let grid = self.grid_mut();
                grid.col = 0;
                grid.line_feed();
            }
            b'M' => self.grid_mut().reverse_index(),
            b'c' => {
                let (cols, rows) = self.size();
                *self = Screen::new(cols, rows);
            }
            _ => {}
        }
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' | b';' | b':' => self.params.push(char::from(byte)),
            b'?' | b'>' | b'<' | b'=' => self.private = true,
            0x20..=0x2f => {}
            0x40..=0x7e => {
                self.state = State::Ground;
                let params: Vec<usize> = self
                    .params
                    .split(';')
                    .map(|value| value.split(':').next().unwrap_or("").parse().unwrap_or(0))
                    .collect();
                let first = params.first().copied().unwrap_or(0);
                let one = first.max(1);
                if self.private {
                    self.private_mode(byte, &params);
                    return;
                }
                let grid = self.grid_mut();
                match byte {
                    b'A' => grid.move_to(grid.row.saturating_sub(one), grid.col),
                    b'B' => grid.move_to(grid.row + one, grid.col),
                    b'C' => grid.move_to(grid.row, grid.col + one),
                    b'D' => grid.move_to(grid.row, grid.col.saturating_sub(one)),
                    b'E' => grid.move_to(grid.row + one, 0),
                    b'F' => grid.move_to(grid.row.saturating_sub(one), 0),
                    b'G' | b'`' => grid.move_to(grid.row, one - 1),
                    b'd' => grid.move_to(one - 1, grid.col),
                    b'H' | b'f' => {
                        let col = params.get(1).copied().unwrap_or(0).max(1);
                        grid.move_to(one - 1, col - 1);
                    }
                    b'J' => grid.erase_display(first),
                    b'K' => grid.erase_line(first),
                    b'L' => grid.insert_lines(one),
                    b'M' => grid.delete_lines(one),
                    b'@' => grid.insert_chars(one),
                    b'P' => grid.delete_chars(one),
                    b'X' => grid.erase_chars(one),
                    b'S' => grid.scroll_up(one),
                    b'T' => grid.scroll_down(one),
                    b'r' => {
                        let top = one - 1;
                        let bottom = params
                            .get(1)
                            .copied()
                            .filter(|value| *value > 0)
                            .unwrap_or(grid.rows)
                            .min(grid.rows)
                            .saturating_sub(1);
                        if top < bottom {
                            grid.scroll_top = top;
                            grid.scroll_bottom = bottom;
                        } else {
                            grid.scroll_top = 0;
                            grid.scroll_bottom = grid.rows.saturating_sub(1);
                        }
                        grid.move_to(0, 0);
                    }
                    b's' => grid.saved_cursor = Some((grid.row, grid.col)),
                    b'u' => {
                        if let Some((row, col)) = grid.saved_cursor {
                            grid.move_to(row, col);
                        }
                    }
                    _ => {}
                }
            }
            _ => self.state = State::Ground,
        }
    }

    fn private_mode(&mut self, byte: u8, params: &[usize]) {
        if byte != b'h' && byte != b'l' {
            return;
        }
        let enable = byte == b'h';
        for &mode in params {
            match mode {
                47 | 1047 | 1049 => {
                    if enable && !self.alternate_active {
                        if mode == 1049 {
                            self.primary.saved_cursor = Some((self.primary.row, self.primary.col));
                        }
                        let (cols, rows) = (self.primary.cols, self.primary.rows);
                        self.alternate = Grid::new(cols, rows);
                        self.alternate_active = true;
                    } else if !enable && self.alternate_active {
                        self.alternate_active = false;
                        if mode == 1049 {
                            if let Some((row, col)) = self.primary.saved_cursor {
                                self.primary.move_to(row, col);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Visible rows with trailing whitespace trimmed.
    pub fn rows(&self) -> Vec<String> {
        self.grid()
            .cells
            .iter()
            .map(|line| line.iter().collect::<String>().trim_end().to_owned())
            .collect()
    }

    /// The untrimmed text of one visible row, or an empty string outside the grid.
    pub fn row_text(&self, row: usize) -> String {
        self.grid()
            .cells
            .get(row)
            .map(|line| line.iter().collect())
            .unwrap_or_default()
    }

    /// Positions (row, column) where `text` starts on the visible screen.
    pub fn find(&self, text: &str) -> Vec<(usize, usize)> {
        let needle: Vec<char> = text.chars().collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (row, line) in self.grid().cells.iter().enumerate() {
            if line.len() < needle.len() {
                continue;
            }
            for col in 0..=line.len() - needle.len() {
                if line[col..col + needle.len()] == needle[..] {
                    found.push((row, col));
                }
            }
        }
        found
    }

    pub fn count(&self, text: &str) -> usize {
        self.find(text).len()
    }

    /// Text in `row` starting at `col` for `width` cells, trimmed at the end.
    pub fn slice(&self, row: usize, col: usize, width: usize) -> String {
        let Some(line) = self.grid().cells.get(row) else {
            return String::new();
        };
        let end = (col + width).min(line.len());
        if col >= end {
            return String::new();
        }
        line[col..end]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// A compact dump for error messages: non-empty rows only.
    pub fn dump(&self) -> String {
        self.rows()
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.is_empty())
            .map(|(index, line)| format!("{index:3}| {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn utf8_length(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_wrap_and_scroll() {
        let mut screen = Screen::new(4, 2);
        screen.feed(b"abcdef\r\nxy");
        assert_eq!(screen.rows(), vec!["ef", "xy"]);
        screen.feed(b"\nz");
        assert_eq!(screen.rows(), vec!["xy", "  z"]);
    }

    #[test]
    fn cursor_positioning_and_erase() {
        let mut screen = Screen::new(10, 3);
        screen.feed(b"\x1b[2J\x1b[2;3HMARK\x1b[1;1Htop\x1b[3;1Hbottom");
        assert_eq!(screen.rows(), vec!["top", "  MARK", "bottom"]);
        assert_eq!(screen.find("MARK"), vec![(1, 2)]);
        screen.feed(b"\x1b[2;1H\x1b[K");
        assert_eq!(screen.count("MARK"), 0);
        screen.feed(b"\x1b[1;1H\x1b[J");
        assert_eq!(screen.rows(), vec!["", "", ""]);
    }

    #[test]
    fn scroll_region_insert_delete() {
        let mut screen = Screen::new(3, 4);
        screen.feed(b"a\r\nb\r\nc\r\nd");
        screen.feed(b"\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(screen.rows(), vec!["a", "c", "", "d"]);
        screen.feed(b"\x1b[2;1H\x1b[L");
        assert_eq!(screen.rows(), vec!["a", "", "c", "d"]);
        screen.feed(b"\x1b[M");
        assert_eq!(screen.rows(), vec!["a", "c", "", "d"]);
        screen.feed(b"\x1b[r\x1b[1;1Hxyz\x1b[1;2H\x1b[P");
        assert_eq!(screen.rows()[0], "xz");
        screen.feed(b"\x1b[1;2H\x1b[@");
        assert_eq!(screen.rows()[0], "x z");
    }

    #[test]
    fn alternate_screen_is_separate_and_restores_primary() {
        let mut screen = Screen::new(8, 2);
        screen.feed(b"primary");
        screen.feed(b"\x1b[?1049h\x1b[Halt");
        assert_eq!(screen.rows(), vec!["alt", ""]);
        screen.feed(b"\x1b[?1049l");
        assert_eq!(screen.rows(), vec!["primary", ""]);
    }

    #[test]
    fn osc_dcs_sgr_and_utf8_are_handled() {
        let mut screen = Screen::new(12, 1);
        screen.feed(b"\x1b]0;title\x07\x1bP+q\x1b\\\x1b[1;31mr\xc3\xa9d\x1b[0m \xe2\x94\x82!");
        assert_eq!(screen.rows(), vec!["r\u{e9}d \u{2502}!"]);
        let mut split = Screen::new(4, 1);
        split.feed(b"\xe2\x94");
        split.feed(b"\x82");
        assert_eq!(split.rows(), vec!["\u{2502}"]);
    }

    #[test]
    fn resize_keeps_content_and_clamps_cursor() {
        let mut screen = Screen::new(6, 2);
        screen.feed(b"hello\r\nworld");
        screen.resize(3, 3);
        assert_eq!(screen.rows(), vec!["hel", "wor", ""]);
        screen.feed(b"!");
        assert_eq!(screen.rows()[1], "wo!");
    }

    #[test]
    fn slices_are_column_anchored() {
        let mut screen = Screen::new(10, 1);
        screen.feed(b"ab MARK xy");
        assert_eq!(screen.slice(0, 3, 4), "MARK");
        assert_eq!(screen.slice(0, 8, 20), "xy");
        assert_eq!(screen.slice(0, 40, 2), "");
    }
}
