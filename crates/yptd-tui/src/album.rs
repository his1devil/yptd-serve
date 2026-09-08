//! Laying several pictures out as one block.
//!
//! OpenIM has no message type that carries more than one picture, so sending
//! four images is four messages. Showing them as four separate blocks, each a
//! dozen rows tall, buries the conversation -- so consecutive pictures from
//! one person are drawn together, the way every chat client does it.
//!
//! The layout is a pure function of the pictures' proportions and the space
//! available, which is what lets the renderer reserve the rows before any
//! picture has been decoded and lets the tests check the geometry without a
//! terminal.

/// Pictures past this many are counted, not drawn. Four fills a block that is
/// still short enough to scroll past.
pub const MAX_TILES: usize = 4;

/// Columns an album may span. Wider than a lone picture, because the tiles
/// share the width.
pub const MAX_COLS: u16 = 64;
/// Columns a single picture may span.
pub const SINGLE_MAX_COLS: u16 = 48;
/// Rows the whole block may occupy, however many pictures are in it.
pub const MAX_ROWS: u16 = 12;

/// A picture's natural size in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source {
    pub width: u32,
    pub height: u32,
}

/// How many pixels one terminal cell covers. Cells are much taller than they
/// are wide, so ignoring this makes every picture look squashed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// Where one picture goes, in cells relative to the block's top left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tile {
    /// Index into the sources this tile draws.
    pub index: usize,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Album {
    pub tiles: Vec<Tile>,
    /// Rows the block occupies, including every tile.
    pub height: u16,
    /// Pictures beyond [`MAX_TILES`], to be reported as "+N".
    pub overflow: usize,
}

impl Album {
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

/// Fits one picture inside a box, preserving its proportions.
///
/// Returns at least one cell in each direction for a picture that has any
/// size at all: a tile rounded away to nothing would leave a gap where the
/// reader expects a picture.
pub fn fit(source: Source, max_cols: u16, max_rows: u16, cell: CellSize) -> (u16, u16) {
    if source.width == 0 || source.height == 0 || cell.width == 0 || cell.height == 0 {
        return (0, 0);
    }
    if max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let box_px_w = f64::from(max_cols) * f64::from(cell.width);
    let box_px_h = f64::from(max_rows) * f64::from(cell.height);
    let scale = (box_px_w / f64::from(source.width))
        .min(box_px_h / f64::from(source.height))
        // Never blow a small picture up past its own size.
        .min(1.0);
    let cols = ((f64::from(source.width) * scale) / f64::from(cell.width)).round();
    let rows = ((f64::from(source.height) * scale) / f64::from(cell.height)).round();
    (
        (cols as u16).clamp(1, max_cols),
        (rows as u16).clamp(1, max_rows),
    )
}

/// Arranges up to [`MAX_TILES`] pictures inside `columns`.
///
/// One fills the width; two sit side by side; three put the first beside a
/// stack of two; four make a square. The shapes are fixed rather than
/// computed from the pictures' proportions, because a layout that reshuffles
/// itself when one picture is a little taller reads as a glitch.
pub fn layout(sources: &[Source], columns: u16, cell: CellSize) -> Album {
    if sources.is_empty() || columns == 0 || cell.width == 0 || cell.height == 0 {
        return Album::default();
    }
    let overflow = sources.len().saturating_sub(MAX_TILES);
    let shown = sources.len().min(MAX_TILES);

    if shown == 1 {
        let width = columns.min(SINGLE_MAX_COLS);
        let (w, h) = fit(sources[0], width, MAX_ROWS, cell);
        if w == 0 || h == 0 {
            return Album::default();
        }
        return Album {
            tiles: vec![Tile { index: 0, x: 0, y: 0, width: w, height: h }],
            height: h,
            overflow,
        };
    }

    let total = columns.min(MAX_COLS);
    let (left, right) = split(total);
    // Two rows of tiles have to share the block's height budget.
    let (top_rows, bottom_rows) = split(MAX_ROWS);

    let tiles = match shown {
        2 => {
            let a = fit(sources[0], left, MAX_ROWS, cell);
            let b = fit(sources[1], right, MAX_ROWS, cell);
            vec![
                Tile { index: 0, x: 0, y: 0, width: a.0, height: a.1 },
                Tile { index: 1, x: left, y: 0, width: b.0, height: b.1 },
            ]
        }
        3 => {
            let a = fit(sources[0], left, MAX_ROWS, cell);
            let b = fit(sources[1], right, top_rows, cell);
            let c = fit(sources[2], right, bottom_rows, cell);
            vec![
                Tile { index: 0, x: 0, y: 0, width: a.0, height: a.1 },
                Tile { index: 1, x: left, y: 0, width: b.0, height: b.1 },
                Tile { index: 2, x: left, y: top_rows, width: c.0, height: c.1 },
            ]
        }
        _ => {
            let a = fit(sources[0], left, top_rows, cell);
            let b = fit(sources[1], right, top_rows, cell);
            let c = fit(sources[2], left, bottom_rows, cell);
            let d = fit(sources[3], right, bottom_rows, cell);
            vec![
                Tile { index: 0, x: 0, y: 0, width: a.0, height: a.1 },
                Tile { index: 1, x: left, y: 0, width: b.0, height: b.1 },
                Tile { index: 2, x: 0, y: top_rows, width: c.0, height: c.1 },
                Tile { index: 3, x: left, y: top_rows, width: d.0, height: d.1 },
            ]
        }
    };

    let height = tiles
        .iter()
        .map(|tile| tile.y.saturating_add(tile.height))
        .max()
        .unwrap_or(0)
        .min(MAX_ROWS);
    if height == 0 {
        return Album::default();
    }
    Album { tiles, height, overflow }
}

/// Halves a span, giving the odd cell to the left or top half.
fn split(total: u16) -> (u16, u16) {
    let first = total.div_ceil(2);
    (first, total.saturating_sub(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typical terminal cell: about twice as tall as it is wide.
    const CELL: CellSize = CellSize { width: 10, height: 20 };

    fn square(n: usize) -> Vec<Source> {
        vec![Source { width: 1000, height: 1000 }; n]
    }

    #[test]
    fn one_picture_keeps_the_single_width_budget() {
        let album = layout(&square(1), 100, CELL);
        assert_eq!(album.tiles.len(), 1);
        assert_eq!(album.overflow, 0);
        assert!(
            album.tiles[0].width <= SINGLE_MAX_COLS,
            "a lone picture must not use the wider album budget"
        );
    }

    #[test]
    fn two_pictures_sit_side_by_side_and_never_overlap() {
        let album = layout(&square(2), 64, CELL);
        assert_eq!(album.tiles.len(), 2);
        let (a, b) = (album.tiles[0], album.tiles[1]);
        assert_eq!(a.y, b.y, "same row");
        assert!(a.x + a.width <= b.x, "the first must end before the second starts");
        assert!(b.x + b.width <= 64, "the block stays inside its columns");
    }

    #[test]
    fn three_pictures_put_one_beside_a_stack_of_two() {
        let album = layout(&square(3), 64, CELL);
        assert_eq!(album.tiles.len(), 3);
        let (big, top, bottom) = (album.tiles[0], album.tiles[1], album.tiles[2]);
        assert_eq!(big.x, 0);
        assert_eq!(top.y, 0);
        assert!(bottom.y >= top.y + top.height, "the stack must not overlap itself");
        assert_eq!(top.x, bottom.x, "the stack shares a left edge");
        assert!(big.x + big.width <= top.x);
    }

    #[test]
    fn four_pictures_make_a_square() {
        let album = layout(&square(4), 64, CELL);
        assert_eq!(album.tiles.len(), 4);
        let rows: Vec<u16> = album.tiles.iter().map(|t| t.y).collect();
        assert_eq!(rows[0], rows[1], "top row");
        assert_eq!(rows[2], rows[3], "bottom row");
        assert!(rows[2] > rows[0]);
        assert_eq!(album.tiles[0].x, album.tiles[2].x, "left column");
        assert_eq!(album.tiles[1].x, album.tiles[3].x, "right column");
    }

    #[test]
    fn more_than_four_draws_four_and_counts_the_rest() {
        let album = layout(&square(9), 64, CELL);
        assert_eq!(album.tiles.len(), MAX_TILES);
        assert_eq!(album.overflow, 5);
    }

    #[test]
    fn no_album_is_taller_than_the_row_budget() {
        for count in 1..=8 {
            for columns in [20u16, 40, 64, 120] {
                let album = layout(&square(count), columns, CELL);
                assert!(
                    album.height <= MAX_ROWS,
                    "{count} pictures at {columns} columns: {} rows",
                    album.height
                );
                for tile in &album.tiles {
                    assert!(
                        tile.y + tile.height <= album.height,
                        "a tile fell outside the reserved rows"
                    );
                    assert!(tile.x + tile.width <= columns.min(MAX_COLS).max(1));
                }
            }
        }
    }

    #[test]
    fn a_tall_picture_and_a_wide_one_both_stay_inside_their_tile() {
        let sources = [
            Source { width: 100, height: 4000 },
            Source { width: 4000, height: 100 },
        ];
        let album = layout(&sources, 64, CELL);
        for tile in &album.tiles {
            assert!(tile.width >= 1 && tile.height >= 1, "nothing rounds away to nothing");
        }
        assert!(album.height <= MAX_ROWS);
    }

    #[test]
    fn fitting_preserves_proportions_in_cell_terms() {
        // A 2:1 picture in a cell twice as tall as wide is square on screen.
        let (cols, rows) = fit(Source { width: 400, height: 200 }, 40, 40, CELL);
        assert_eq!(cols, 40);
        assert_eq!(rows, 10, "200px / 20px per row");
    }

    #[test]
    fn a_small_picture_is_not_blown_up() {
        let (cols, rows) = fit(Source { width: 40, height: 40 }, 40, 40, CELL);
        assert_eq!((cols, rows), (4, 2), "40px wide is 4 cells, 40px tall is 2 rows");
    }

    #[test]
    fn degenerate_input_yields_nothing_rather_than_a_panic() {
        assert!(layout(&[], 64, CELL).is_empty());
        assert!(layout(&square(2), 0, CELL).is_empty());
        assert!(layout(&square(2), 64, CellSize { width: 0, height: 0 }).is_empty());
        assert_eq!(fit(Source { width: 0, height: 10 }, 10, 10, CELL), (0, 0));
    }
}
