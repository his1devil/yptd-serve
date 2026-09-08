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

/// Arranges pictures as one row of identical tiles.
///
/// Identical, not proportional: tiles sized to each picture's own shape leave
/// different gaps and read as a ragged pile rather than one block. A single
/// picture is the exception -- there is nothing to line it up with, so it
/// keeps its own proportions and is shown whole.
pub fn layout(sources: &[Source], columns: u16, cell: CellSize) -> Album {
    if sources.is_empty() || columns == 0 || cell.width == 0 || cell.height == 0 {
        return Album::default();
    }
    let overflow = sources.len().saturating_sub(MAX_TILES);
    let shown = sources.len().min(MAX_TILES);

    if shown == 1 {
        let (w, h) = fit(sources[0], columns.min(SINGLE_MAX_COLS), MAX_ROWS, cell);
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
    // One gap column between tiles, so two pictures do not touch.
    let gaps = (shown as u16).saturating_sub(1);
    let usable = total.saturating_sub(gaps);
    let each = usable / shown as u16;
    if each == 0 {
        return Album::default();
    }
    // Square on screen, which is why the cell's own proportions come into it:
    // a cell is about twice as tall as it is wide.
    let rows = (u32::from(each) * u32::from(cell.width) / u32::from(cell.height).max(1)) as u16;
    let rows = rows.clamp(1, MAX_ROWS);

    let tiles = (0..shown)
        .map(|index| Tile {
            index,
            x: index as u16 * (each + 1),
            y: 0,
            width: each,
            height: rows,
        })
        .collect();
    Album { tiles, height: rows, overflow }
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
    fn one_picture_keeps_its_own_proportions_and_the_single_width_budget() {
        let album = layout(&square(1), 100, CELL);
        assert_eq!(album.tiles.len(), 1);
        assert!(album.tiles[0].width <= SINGLE_MAX_COLS);
    }

    #[test]
    fn several_pictures_get_tiles_of_identical_size() {
        // The point of the row: nothing about one picture's proportions may
        // make its tile bigger or smaller than its neighbour's.
        for count in 2..=MAX_TILES {
            let mixed: Vec<Source> = (0..count)
                .map(|i| Source { width: 400 + 900 * i as u32, height: 1600 - 300 * i as u32 })
                .collect();
            let album = layout(&mixed, 64, CELL);
            assert_eq!(album.tiles.len(), count);
            let first = album.tiles[0];
            for tile in &album.tiles {
                assert_eq!(tile.width, first.width, "{count} pictures");
                assert_eq!(tile.height, first.height, "{count} pictures");
                assert_eq!(tile.y, 0, "one row");
            }
        }
    }

    #[test]
    fn tiles_sit_in_order_left_to_right_without_touching() {
        let album = layout(&square(4), 64, CELL);
        for pair in album.tiles.windows(2) {
            assert!(
                pair[0].x + pair[0].width < pair[1].x,
                "tiles must not touch: {pair:?}"
            );
        }
    }

    #[test]
    fn more_than_four_draws_four_and_counts_the_rest() {
        let album = layout(&square(9), 64, CELL);
        assert_eq!(album.tiles.len(), MAX_TILES);
        assert_eq!(album.overflow, 5);
    }

    #[test]
    fn no_album_is_taller_or_wider_than_its_budget() {
        for count in 1..=8 {
            for columns in [20u16, 40, 64, 120] {
                let album = layout(&square(count), columns, CELL);
                assert!(album.height <= MAX_ROWS, "{count} at {columns}");
                for tile in &album.tiles {
                    assert!(tile.y + tile.height <= album.height);
                    assert!(tile.x + tile.width <= columns.min(MAX_COLS).max(1));
                }
            }
        }
    }

    #[test]
    fn a_row_too_narrow_for_a_tile_each_draws_nothing() {
        // Better an attachment line on its own than four one-column smears.
        assert!(layout(&square(4), 4, CELL).is_empty());
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
