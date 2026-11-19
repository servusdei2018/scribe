//! Grid and simple table parsing.
//!
//! The grid-table cell-walking algorithm and the simple-table column/row
//! detection are ports of `docutils/parsers/rst/tableparser.py`
//! (`GridTableParser` and `SimpleTableParser`). The resulting data structure
//! is turned into `table/tgroup/colspec/thead/tbody/row/entry` nodes exactly as
//! `Body.build_table()` does.
//!
//! Table cell contents are parsed recursively with the same block parser by
//! temporarily swapping in synthetic [`SourceLine`](crate::lexer::SourceLine)s
//! whose spans point back into the original table rows.

use std::borrow::Cow;
use std::collections::BTreeSet;

use scribe_ast::NodeKind;

use crate::error::RstError;
use crate::lexer::SourceLine;
use crate::parser::BlockParser;
use crate::RstResult;

/// Pattern `\+[-+]+-\+ *$` from `Body.grid_table_top_pat`.
pub(crate) fn is_grid_table_top(content: &str) -> bool {
    let trimmed = content.trim_end();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 5 || bytes.first() != Some(&b'+') || bytes.last() != Some(&b'+') {
        return false;
    }
    bytes[1..bytes.len() - 1].iter().all(|&b| b == b'-' || b == b'+')
}

/// Pattern `=+( +=+)+ *$` from `Body.simple_table_top_pat`.
pub(crate) fn is_simple_table_top(content: &str) -> bool {
    let trimmed = content.trim_end();
    let bytes = trimmed.as_bytes();
    if !bytes.iter().all(|&b| b == b'=' || b == b' ') {
        return false;
    }
    let mut runs = 0usize;
    let mut in_run = false;
    for &b in bytes {
        if b == b'=' && !in_run {
            runs += 1;
            in_run = true;
        } else if b == b' ' {
            in_run = false;
        }
    }
    runs >= 2 && bytes.last() == Some(&b'=')
}

/// One parsed table cell: `(morerows, morecols, text_lines)`.
type Cell = (usize, usize, Vec<String>);
/// Table structure: `(column widths, head rows, body rows)`.
type TableData = (Vec<usize>, Vec<Vec<Option<Cell>>>, Vec<Vec<Option<Cell>>>);

impl<'a> BlockParser<'a> {
    // ----- grid tables -----------------------------------------------------

    pub(crate) fn parse_grid_table(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let (block_end, block) = self.collect_table_block(base_indent);
        if block.len() < 3 {
            return Err(RstError::Table {
                message: "grid table is too short".into(),
                span: self.miette_span(self.span_of(start, block_end)),
            });
        }
        let top = block[0].trim_end();
        let bottom = block.last().map_or("", |line| line.trim_end());
        if !is_grid_table_top(bottom) || bottom.len() != top.len() {
            return Err(RstError::Table {
                message: "bottom border missing or corrupt".into(),
                span: self.miette_span(self.span_of(start, block_end)),
            });
        }
        if block.iter().any(|line| {
            let l = line.trim_end();
            l.len() != top.len()
                || !(l.starts_with('+') || l.starts_with('|'))
                || !(l.ends_with('+') || l.ends_with('|'))
        }) {
            return Err(RstError::Table {
                message: "right border not aligned or missing".into(),
                span: self.miette_span(self.span_of(start, block_end)),
            });
        }

        let matrix: Vec<Vec<char>> = block
            .iter()
            .map(|line| line.trim_end().chars().collect())
            .collect();
        let data = parse_grid_matrix(&matrix).ok_or_else(|| RstError::Table {
            message: "malformed table; parse incomplete".into(),
            span: self.miette_span(self.span_of(start, block_end)),
        })?;

        self.build_table(parent, data, start, block_end, base_indent, &block)?;
        self.pos = block_end;
        Ok(())
    }

    // ----- simple tables ---------------------------------------------------

    pub(crate) fn parse_simple_table(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let (block_end, block) = self.collect_table_block(base_indent);
        let top = block.first().map_or("", |line| line.trim_end());
        let width = top.len();
        let borders: Vec<usize> = (1..block.len())
            .filter(|&i| {
                let line = block[i].trim_end();
                is_simple_border(line) && line.len() == width
            })
            .collect();
        let Some(&bottom_index) = borders.last() else {
            return Err(RstError::Table {
                message: "no bottom table border found".into(),
                span: self.miette_span(self.span_of(start, block_end)),
            });
        };
        let head_sep = borders.first().copied().filter(|&i| i < bottom_index);
        let table_block = &block[..=bottom_index];

        let columns = parse_simple_columns(&table_block[0].trim_end().replace('=', "-"));
        if columns.len() < 2 {
            return Err(RstError::Table {
                message: "table top border does not describe two or more columns".into(),
                span: self.miette_span(self.span_of(start, block_end)),
            });
        }

        let mut rows: Vec<Vec<Option<Cell>>> = Vec::new();
        let mut head_rows = 0usize;
        let mut pending: Vec<String> = Vec::new();

        // Extracts one row from the accumulated text lines.
        fn finish_row(
            rows: &mut Vec<Vec<Option<Cell>>>,
            pending: &[String],
            columns: &[(usize, usize)],
            span_columns: Option<&[(usize, usize)]>,
        ) {
            if pending.is_empty() {
                return;
            }
            let cell_columns: &[(usize, usize)] = span_columns.unwrap_or(columns);
            let mut row = Vec::with_capacity(cell_columns.len());
            for (c, &(cs, ce)) in cell_columns.iter().enumerate() {
                let text = pending
                    .iter()
                    .map(|line| {
                        let line = line.trim_end();
                        // Docutils allows the rightmost column to overflow.
                        let end = if c + 1 == cell_columns.len() {
                            line.len().max(ce)
                        } else {
                            ce
                        };
                        column_slice(line, cs, end)
                    })
                    .collect::<Vec<_>>();
                let morecols = span_columns.map_or(0, |spans| expected_morecols(columns, c, spans[c].0, spans[c].1));
                row.push(Some((0, morecols, text)));
            }
            rows.push(row);
        }

        for (i, block_line) in table_block.iter().enumerate().take(bottom_index + 1).skip(1) {
            let line = block_line.trim_end().to_owned();
            if is_simple_border(&line) && line.len() == width {
                finish_row(&mut rows, &pending, &columns, None);
                pending.clear();
                if head_sep == Some(i) {
                    head_rows = rows.len();
                }
                continue;
            }
            let span_columns = is_simple_span(&line) && line.len() == width;
            if span_columns {
                let span = parse_simple_columns(&line);
                finish_row(&mut rows, &pending, &columns, Some(&span));
                pending.clear();
                continue;
            }
            let first_col = column_slice(&line, columns[0].0, columns[0].1);
            if !first_col.trim().is_empty() && !pending.is_empty() {
                finish_row(&mut rows, &pending, &columns, None);
                pending.clear();
            }
            pending.push(line);
        }
        finish_row(&mut rows, &pending, &columns, None);

        let col_widths: Vec<usize> = columns.iter().map(|&(s, e)| e - s).collect();
        let body_rows = rows.split_off(head_rows.min(rows.len()));
        let data = (col_widths, rows, body_rows);
        self.build_table(parent, data, start, block_end, base_indent, table_block)?;
        self.pos = block_end;
        Ok(())
    }

    /// Collects the contiguous table text block, returning its end index and lines.
    fn collect_table_block(&self, base_indent: usize) -> (usize, Vec<String>) {
        let mut end = self.pos;
        let mut block = Vec::new();
        for i in self.pos..self.limit.min(self.lines.len()) {
            let line = &self.lines[i];
            if line.blank {
                break;
            }
            if line.indent_cols < base_indent {
                break;
            }
            block.push(self.content_at(i, base_indent).to_owned());
            end = i + 1;
        }
        (end, block)
    }

    /// Builds the AST nodes for parsed table data.
    fn build_table(
        &mut self,
        parent: scribe_ast::NodeId,
        (widths, head, body): TableData,
        start: usize,
        block_end: usize,
        base_indent: usize,
        block: &[String],
    ) -> RstResult<()> {
        let span = self.span_of(start, block_end);
        let line_no = self.line_number(start);
        let table = self.add(NodeKind::Table, parent, span, line_no)?;
        let tgroup = self.add(NodeKind::Tgroup, table, span, line_no)?;
        self.set_int(tgroup, "cols", widths.len() as i64)?;
        for width in &widths {
            let colspec = self.add(NodeKind::Colspec, tgroup, span, line_no)?;
            self.set_int(colspec, "colwidth", *width as i64)?;
        }
        if !head.is_empty() {
            let thead = self.add(NodeKind::Thead, tgroup, span, line_no)?;
            for row in head {
                self.build_table_row(thead, row, base_indent, block)?;
            }
        }
        let tbody = self.add(NodeKind::Tbody, tgroup, span, line_no)?;
        for row in body {
            self.build_table_row(tbody, row, base_indent, block)?;
        }
        Ok(())
    }

    fn build_table_row(
        &mut self,
        parent: scribe_ast::NodeId,
        row: Vec<Option<Cell>>,
        base_indent: usize,
        block: &[String],
    ) -> RstResult<()> {
        let span = self.line_span(self.pos.min(self.lines.len().saturating_sub(1)));
        let line_no = self.line_number(self.pos.min(self.lines.len().saturating_sub(1)));
        let row_node = self.add(NodeKind::Row, parent, span, line_no)?;
        for cell in row.into_iter().flatten() {
            let (morerows, morecols, lines) = cell;
            let entry = self.add(NodeKind::Entry, row_node, span, line_no)?;
            if morerows > 0 {
                self.set_int(entry, "morerows", morerows as i64)?;
            }
            if morecols > 0 {
                self.set_int(entry, "morecols", morecols as i64)?;
            }
            let cell_lines = self.make_cell_lines(base_indent, lines);
            let saved_lines = std::mem::take(&mut self.lines);
            let saved_pos = self.pos;
            let saved_limit = self.limit;
            self.lines = cell_lines;
            self.pos = 0;
            self.limit = usize::MAX;
            let result = self.parse_blocks(entry, 0);
            self.lines = saved_lines;
            self.pos = saved_pos;
            self.limit = saved_limit;
            result?;
        }
        let _ = block;
        Ok(())
    }

    /// Builds synthetic source lines for a table cell's text.
    fn make_cell_lines(&self, base_indent: usize, lines: Vec<String>) -> Vec<SourceLine<'a>> {
        // Table padding contributes a common leading space on every cell row;
        // Docutils' statemachine strips it as the block indentation.
        let common_indent = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.bytes().take_while(|&b| b == b' ').count())
            .min()
            .unwrap_or(0);
        lines
            .into_iter()
            .enumerate()
            .map(|(i, original)| {
                let text = original
                    .trim_end()
                    .get(common_indent.min(original.trim_end().len())..)
                    .unwrap_or("")
                    .to_owned();
                let ind = text.bytes().take_while(|&b| b == b' ').count();
                let blank = text.trim().is_empty();
                let start = self
                    .line(self.pos + i)
                    .map_or(0, |line| line.start.saturating_add(base_indent as u32));
                SourceLine {
                    number: self.line_number(self.pos + i) as usize,
                    raw: "",
                    text: Cow::Owned(text),
                    start,
                    end: start,
                    next: start,
                    eol: "",
                    indent_cols: ind,
                    indent_bytes: ind,
                    blank,
                }
            })
            .collect()
    }
}

// ----- pure grid-table data extraction -------------------------------------

/// Ports `GridTableParser.parse_table()` + `structure_from_cells()`.
fn parse_grid_matrix(matrix: &[Vec<char>]) -> Option<TableData> {
    let bottom = matrix.len().checked_sub(1)?;
    if bottom < 2 {
        return None;
    }
    let right = matrix.first()?.len().checked_sub(1)?;
    let mut done = vec![-1isize; right + 1];
    let mut cells: Vec<(usize, usize, usize, usize, Vec<String>)> = Vec::new();
    let mut rowseps: BTreeSet<usize> = BTreeSet::new();
    let mut colseps: BTreeSet<usize> = BTreeSet::new();
    rowseps.insert(0);
    colseps.insert(0);
    let mut corners = vec![(0usize, 0usize)];
    while let Some((top, left)) = corners.first().copied() {
        corners.remove(0);
        if top == bottom || left == right || (done[left] >= 0 && top <= done[left] as usize) {
            continue;
        }
        let (cell_bottom, cell_right, rows, cols) = scan_cell(matrix, top, left)?;
        rowseps.extend(rows);
        colseps.extend(cols);
        for slot in done.iter_mut().take(cell_right).skip(left) {
            *slot = cell_bottom as isize - 1;
        }
        let content = (top + 1..cell_bottom)
            .map(|r| matrix[r][left + 1..cell_right].iter().collect::<String>())
            .collect();
        cells.push((top, left, cell_bottom, cell_right, content));
        corners.push((top, cell_right));
        corners.push((cell_bottom, left));
        corners.sort_unstable();
    }
    // All text columns must have been consumed.
    let last = bottom - 1;
    if (0..right).any(|col| done[col] != last as isize) {
        return None;
    }

    let row_indexes: Vec<usize> = rowseps.iter().copied().collect();
    let col_indexes: Vec<usize> = colseps.iter().copied().collect();
    let widths: Vec<usize> = col_indexes
        .windows(2)
        .map(|pair| pair[1] - pair[0] - 1)
        .collect();
    let row_count = row_indexes.len().saturating_sub(1);
    let col_count = col_indexes.len().saturating_sub(1);
    let mut rows: Vec<Vec<Option<Cell>>> = vec![vec![None; col_count]; row_count];
    let head_sep = find_head_separator(matrix);
    for (top, left, cell_bottom, cell_right, content) in cells {
        let rownum = row_indexes.iter().position(|&x| x == top)?;
        let colnum = col_indexes.iter().position(|&x| x == left)?;
        let morerows = row_indexes.iter().position(|&x| x == cell_bottom)? - rownum - 1;
        let morecols = col_indexes.iter().position(|&x| x == cell_right)? - colnum - 1;
        rows[rownum][colnum] = Some((morerows, morecols, content));
    }
    let head_count = head_sep
        .and_then(|sep| row_indexes.iter().position(|&x| x == sep))
        .unwrap_or(0);
    let body = rows.split_off(head_count);
    Some((widths, rows, body))
}

/// Finds a `+===+` head/body separator row index.
fn find_head_separator(matrix: &[Vec<char>]) -> Option<usize> {
    for (i, row) in matrix.iter().enumerate() {
        if row.first() != Some(&'+') || row.last() != Some(&'+') {
            continue;
        }
        let inner = &row[1..row.len() - 1];
        if !inner.is_empty()
            && inner.iter().all(|&c| c == '=' || c == '+')
            && inner.contains(&'=')
        {
            return Some(i);
        }
    }
    None
}

/// Traces one rectangular cell: `(bottom, right, rowseps, colseps)`.
fn scan_cell(
    matrix: &[Vec<char>],
    top: usize,
    left: usize,
) -> Option<(usize, usize, Vec<usize>, Vec<usize>)> {
    let right_edge = matrix[top].len() - 1;
    let mut colseps = Vec::new();
    for i in (left + 1)..=right_edge {
        match matrix[top][i] {
            '+' => {
                colseps.push(i);
                if let Some((bottom, rows, cols)) = scan_down(matrix, top, left, i) {
                    colseps.extend(cols);
                    return Some((bottom, i, rows, colseps));
                }
            }
            '-' | '=' => {}
            _ => return None,
        }
    }
    None
}

fn scan_down(
    matrix: &[Vec<char>],
    top: usize,
    left: usize,
    right: usize,
) -> Option<(usize, Vec<usize>, Vec<usize>)> {
    let bottom_edge = matrix.len() - 1;
    let mut rowseps = Vec::new();
    for i in (top + 1)..=bottom_edge {
        match matrix[i][right] {
            '+' => {
                rowseps.push(i);
                if let Some((rows, cols)) = scan_left(matrix, top, left, i, right) {
                    rowseps.extend(rows);
                    return Some((i, rowseps, cols));
                }
            }
            '|' => {}
            _ => return None,
        }
    }
    None
}

fn scan_left(
    matrix: &[Vec<char>],
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut colseps = Vec::new();
    for i in (left + 1..right).rev() {
        match matrix[bottom][i] {
            '+' => colseps.push(i),
            '-' | '=' => {}
            _ => return None,
        }
    }
    if matrix[bottom][left] != '+' {
        return None;
    }
    scan_up(matrix, top, left, bottom, right).map(|rows| (rows, colseps))
}

fn scan_up(
    matrix: &[Vec<char>],
    top: usize,
    left: usize,
    bottom: usize,
    _right: usize,
) -> Option<Vec<usize>> {
    let mut rowseps = Vec::new();
    for i in (top + 1..bottom).rev() {
        match matrix[i][left] {
            '+' => rowseps.push(i),
            '|' => {}
            _ => return None,
        }
    }
    Some(rowseps)
}

// ----- simple-table helpers ------------------------------------------------

fn is_simple_border(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b == b'=' || b == b' ') && line.contains('=')
}

fn is_simple_span(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b == b'-' || b == b' ') && line.contains('-')
}

/// Returns `(start, end)` byte ranges of each `=`-run in a border line.
fn parse_simple_columns(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut cols = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'-' || bytes[i] == b'=' {
            let start = i;
            while i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'=') {
                i += 1;
            }
            cols.push((start, i));
        } else {
            i += 1;
        }
    }
    cols
}

fn column_slice(line: &str, start: usize, end: usize) -> String {
    let len = line.len();
    if start >= len {
        return String::new();
    }
    line[start..end.min(len)].to_owned()
}

/// Number of extra columns a span-line cell covers relative to the top columns.
fn expected_morecols(columns: &[(usize, usize)], index: usize, start: usize, end: usize) -> usize {
    if let Some(&(cs, ce)) = columns.get(index) {
        if cs == start && ce == end {
            return 0;
        }
    }
    columns
        .iter()
        .skip(index)
        .take_while(|&&(cs, ce)| cs >= start && ce <= end)
        .count()
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_detection() {
        assert!(is_grid_table_top("+---+---+"));
        assert!(!is_grid_table_top("+===+===+   ")); // head separator is not a top border
        assert!(is_grid_table_top("+---+--+")); // two columns of widths 3 and 2
        assert!(is_simple_table_top("=== ==="));
        assert!(is_simple_table_top("=====  =====  ====="));
        assert!(is_simple_table_top("=== =")); // two columns, second one char wide
        assert!(!is_simple_table_top("=== =x"));
    }

    #[test]
    fn grid_matrix_with_column_span() {
        let matrix: Vec<Vec<char>> = [
            "+---+---+",
            "| a | b |",
            "+===+===+",
            "| 1 | 2 |",
            "+---+---+",
        ]
        .iter()
        .map(|line| line.chars().collect())
        .collect();
        let (widths, head, body) = parse_grid_matrix(&matrix).expect("parse");
        assert_eq!(widths, vec![3, 3]);
        assert_eq!(head.len(), 1);
        assert_eq!(body.len(), 1);
        assert_eq!(head[0][0].as_ref().map(|c| &c.2), Some(&vec![" a ".to_owned()]));
    }

    #[test]
    fn grid_matrix_with_row_and_col_span() {
        let matrix: Vec<Vec<char>> = [
            "+---+---+---+",
            "| a | b     |",
            "+---+   +---+",
            "| c |   | d |",
            "+---+---+---+",
        ]
        .iter()
        .map(|line| line.chars().collect())
        .collect();
        let (_, _, body) = parse_grid_matrix(&matrix).expect("parse");
        assert_eq!(body.len(), 2);
        assert_eq!(body[0][1].as_ref().map(|c| (c.0, c.1)), Some((1, 1)));
    }

    #[test]
    fn simple_columns() {
        assert_eq!(parse_simple_columns("=====  ====="), vec![(0, 5), (7, 12)]);
    }
}
