//! Cell-by-cell grid comparison, for golden tests and cross-checks between emulators.

use super::{Cell, Grid};

/// One cell that differs between two grids. Rows and columns are zero-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellDiff {
    /// Row.
    pub row: u16,
    /// Column.
    pub col: u16,
    /// The cell in the first grid.
    pub left: Cell,
    /// The cell in the second grid.
    pub right: Cell,
}

/// Every cell that differs, over the rows and columns both grids have, in row-major order.
/// The cursor is not compared.
#[must_use]
pub fn diff(left: &Grid, right: &Grid) -> Vec<CellDiff> {
    let rows = left.size().rows().min(right.size().rows());
    let cols = usize::from(left.size().cols().min(right.size().cols()));
    let mut out = Vec::new();
    for row in 0..rows {
        let pairs = left.row(row)[..cols].iter().zip(&right.row(row)[..cols]);
        for (col, (l, r)) in (0u16..).zip(pairs) {
            if l != r {
                out.push(CellDiff {
                    row,
                    col,
                    left: l.clone(),
                    right: r.clone(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::diff;
    use crate::emulator::{Cell, Cursor, Grid, Size};
    use proptest::prelude::*;

    fn grid_of(cols: u16, rows: u16, text: &[(u16, u16, &str)]) -> Grid {
        let size = Size::new(cols, rows).unwrap();
        let mut cells = vec![Cell::blank(); size.cells()];
        for &(r, c, t) in text {
            cells[usize::from(r) * usize::from(cols) + usize::from(c)].text = t.into();
        }
        Grid::new(size, cells, Cursor::default()).unwrap()
    }

    #[test]
    fn reports_each_differing_cell_in_order() {
        let a = grid_of(3, 2, &[(0, 1, "x"), (1, 2, "y")]);
        let b = grid_of(3, 2, &[(1, 2, "z")]);
        let d = diff(&a, &b);
        let at: Vec<(u16, u16)> = d.iter().map(|d| (d.row, d.col)).collect();
        assert_eq!(at, vec![(0, 1), (1, 2)]);
        assert_eq!(d[1].left.text, "y");
        assert_eq!(d[1].right.text, "z");
    }

    #[test]
    fn compares_only_the_shared_area() {
        let a = grid_of(2, 1, &[]);
        let b = grid_of(3, 2, &[(1, 0, "q"), (0, 2, "w")]);
        assert!(diff(&a, &b).is_empty());
    }

    proptest! {
        #[test]
        fn a_grid_equals_itself(cols in 1u16..20, rows in 1u16..10, r in 0u16..10, c in 0u16..20) {
            let g = grid_of(cols, rows, &[(r % rows, c % cols, "k")]);
            prop_assert!(diff(&g, &g).is_empty());
        }
    }
}
