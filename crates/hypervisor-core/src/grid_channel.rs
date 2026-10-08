//! Pure grid-frame construction from the holder's active screen.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::channel::{Frame, FrameError, WireSize};
use crate::emulator::{Attrs, Cell, CellWidth, Color, Cursor, Grid, Modes, Underline};

/// A cell's visible content and OSC 8 reference. The URI table is in the same frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridCell {
    pub cluster: String,
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Color,
    pub attributes: Attrs,
    pub underline: Underline,
    pub hyperlink_id: Option<u32>,
    pub width: CellWidth,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hyperlink {
    pub id: u32,
    pub uri: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowDiff {
    pub row: u16,
    pub cells: Vec<GridCell>,
}

/// Full active screen or complete replacement rows relative to a prior frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GridFrame {
    Full {
        frame: u64,
        output_sequence: u64,
        size: WireSize,
        cells: Vec<GridCell>,
        cursor: Cursor,
        modes: Modes,
        hyperlinks: Vec<Hyperlink>,
    },
    Diff {
        frame: u64,
        base_frame: u64,
        output_sequence: u64,
        size: WireSize,
        rows: Vec<RowDiff>,
        cursor: Cursor,
        modes: Modes,
        hyperlinks: Vec<Hyperlink>,
    },
}

impl GridFrame {
    /// Reject shapes a client could not apply unambiguously.
    ///
    /// # Errors
    /// Returns `InvalidValue` for an invalid frame number, row, cursor, or link reference.
    pub fn validate(&self) -> Result<(), FrameError> {
        let (frame, cursor, cells, hyperlinks) = match self {
            Self::Full {
                frame,
                size,
                cells,
                cursor,
                hyperlinks,
                ..
            } => {
                if size.cols == 0
                    || size.rows == 0
                    || cells.len() != usize::from(size.cols) * usize::from(size.rows)
                    || cursor.col >= size.cols
                    || cursor.row >= size.rows
                {
                    return Err(FrameError::InvalidValue);
                }
                (*frame, cursor, cells.iter().collect::<Vec<_>>(), hyperlinks)
            }
            Self::Diff {
                frame,
                base_frame,
                size,
                rows,
                cursor,
                hyperlinks,
                ..
            } => {
                if *base_frame == 0 || base_frame.checked_add(1) != Some(*frame) {
                    return Err(FrameError::InvalidValue);
                }
                if size.cols == 0
                    || size.rows == 0
                    || cursor.col >= size.cols
                    || cursor.row >= size.rows
                {
                    return Err(FrameError::InvalidValue);
                }
                let mut last = None;
                for row in rows {
                    if row.cells.is_empty()
                        || last.is_some_and(|prior| prior >= row.row)
                        || row.row >= size.rows
                        || row.cells.len() != usize::from(size.cols)
                    {
                        return Err(FrameError::InvalidValue);
                    }
                    last = Some(row.row);
                }
                (
                    *frame,
                    cursor,
                    rows.iter().flat_map(|row| &row.cells).collect::<Vec<_>>(),
                    hyperlinks,
                )
            }
        };
        if frame == 0
            || hyperlinks
                .iter()
                .any(|link| link.id == 0 || link.uri.is_empty())
            || hyperlinks.windows(2).any(|pair| pair[0].id >= pair[1].id)
            || cells.iter().any(|cell| {
                cell.cluster.is_empty()
                    || cell
                        .hyperlink_id
                        .is_some_and(|id| !hyperlinks.iter().any(|link| link.id == id))
            })
        {
            return Err(FrameError::InvalidValue);
        }
        let _ = cursor;
        Ok(())
    }
}

/// Per-channel frame number and rate-limit state. The caller supplies monotonic time.
pub struct GridEncoder {
    frame: u64,
    ids: BTreeMap<String, u32>,
    previous: Option<Grid>,
    previous_modes: Modes,
    sent_at: Option<Duration>,
    min_interval: Duration,
}

impl GridEncoder {
    /// `max_frames_per_second` is a client cap; zero is invalid.
    ///
    /// # Errors
    /// Returns `InvalidValue` for zero.
    pub fn new(max_frames_per_second: Option<u16>) -> Result<Self, FrameError> {
        if max_frames_per_second == Some(0) {
            return Err(FrameError::InvalidValue);
        }
        Ok(Self {
            frame: 0,
            ids: BTreeMap::new(),
            previous: None,
            previous_modes: Modes::default(),
            sent_at: None,
            min_interval: max_frames_per_second.map_or(Duration::ZERO, |rate| {
                Duration::from_nanos(1_000_000_000_u64.div_ceil(u64::from(rate)))
            }),
        })
    }

    /// Whether another frame may be emitted at `now`.
    ///
    /// # Errors
    /// Returns `InvalidValue` when time moves backwards.
    pub fn due(&self, now: Duration) -> Result<bool, FrameError> {
        if self.sent_at.is_some_and(|at| now < at) {
            return Err(FrameError::InvalidValue);
        }
        Ok(self
            .sent_at
            .is_none_or(|at| now.saturating_sub(at) >= self.min_interval))
    }

    /// Build the next full or row-diff frame; unchanged and throttled states emit nothing.
    ///
    /// # Errors
    /// Returns a framing error before advancing state if the frame is invalid or too large.
    pub fn poll(
        &mut self,
        now: Duration,
        grid: Grid,
        modes: Modes,
        output_sequence: u64,
        force_full: bool,
    ) -> Result<Option<GridFrame>, FrameError> {
        if !self.due(now)? {
            return Ok(None);
        }
        let full = force_full
            || self
                .previous
                .as_ref()
                .is_none_or(|old| old.size() != grid.size());
        let changed_rows: Vec<u16> = if full {
            Vec::new()
        } else {
            (0..grid.size().rows())
                .filter(|&row| {
                    self.previous
                        .as_ref()
                        .is_some_and(|old| old.row(row) != grid.row(row))
                })
                .collect()
        };
        if !full
            && changed_rows.is_empty()
            && self
                .previous
                .as_ref()
                .is_some_and(|old| old.cursor() == grid.cursor())
            && self.previous_modes == modes
        {
            return Ok(None);
        }
        let number = self.frame.checked_add(1).ok_or(FrameError::InvalidValue)?;
        let (next_ids, hyperlinks) = links(&grid, &self.ids)?;
        let ids: BTreeMap<&str, u32> = hyperlinks
            .iter()
            .map(|link| (link.uri.as_str(), link.id))
            .collect();
        let frame = if full {
            GridFrame::Full {
                frame: number,
                output_sequence,
                size: WireSize {
                    cols: grid.size().cols(),
                    rows: grid.size().rows(),
                },
                cells: grid
                    .cells()
                    .iter()
                    .map(|cell| wire_cell(cell, &ids))
                    .collect(),
                cursor: grid.cursor(),
                modes,
                hyperlinks,
            }
        } else {
            GridFrame::Diff {
                frame: number,
                base_frame: self.frame,
                output_sequence,
                size: WireSize {
                    cols: grid.size().cols(),
                    rows: grid.size().rows(),
                },
                rows: changed_rows
                    .into_iter()
                    .map(|row| RowDiff {
                        row,
                        cells: grid
                            .row(row)
                            .iter()
                            .map(|cell| wire_cell(cell, &ids))
                            .collect(),
                    })
                    .collect(),
                cursor: grid.cursor(),
                modes,
                hyperlinks,
            }
        };
        Frame::Grid(frame.clone()).encode()?;
        self.frame = number;
        self.ids = next_ids;
        self.previous = Some(grid);
        self.previous_modes = modes;
        self.sent_at = Some(now);
        Ok(Some(frame))
    }
}

fn links(
    grid: &Grid,
    known: &BTreeMap<String, u32>,
) -> Result<(BTreeMap<String, u32>, Vec<Hyperlink>), FrameError> {
    let mut ids = known.clone();
    let mut uris: Vec<&str> = grid
        .cells()
        .iter()
        .filter_map(|cell| cell.hyperlink.as_deref())
        .collect();
    uris.sort_unstable();
    uris.dedup();
    let mut links = Vec::with_capacity(uris.len());
    for uri in uris {
        let id = if let Some(id) = ids.get(uri) {
            *id
        } else {
            let id = u32::try_from(ids.len() + 1).map_err(|_| FrameError::InvalidValue)?;
            ids.insert(uri.to_owned(), id);
            id
        };
        links.push(Hyperlink {
            id,
            uri: uri.to_owned(),
        });
    }
    links.sort_unstable_by_key(|link| link.id);
    Ok((ids, links))
}

fn wire_cell(cell: &Cell, ids: &BTreeMap<&str, u32>) -> GridCell {
    GridCell {
        cluster: cell.text.clone(),
        foreground: cell.fg,
        background: cell.bg,
        underline_color: cell.underline_color,
        attributes: cell.attrs,
        underline: cell.underline,
        hyperlink_id: cell
            .hyperlink
            .as_deref()
            .and_then(|uri| ids.get(uri).copied()),
        width: cell.width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{Screen, Size};
    use proptest::prelude::*;

    fn screen(rows: &[&str], cursor: Cursor) -> Grid {
        let cols = u16::try_from(rows[0].len()).unwrap();
        let size = Size::new(cols, u16::try_from(rows.len()).unwrap()).unwrap();
        let cells = rows
            .iter()
            .flat_map(|row| {
                row.chars().map(|ch| Cell {
                    text: ch.to_string(),
                    ..Cell::blank()
                })
            })
            .collect();
        Grid::new(size, cells, cursor).unwrap()
    }

    #[test]
    fn full_diff_cursor_mode_and_resize_frames_round_trip() {
        let mut encoder = GridEncoder::new(None).unwrap();
        let original = screen(&["ab", "cd"], Cursor::default());
        let first = encoder
            .poll(Duration::ZERO, original.clone(), Modes::default(), 3, false)
            .unwrap()
            .unwrap();
        assert!(
            matches!(&first, GridFrame::Full { frame: 1, output_sequence: 3, cells, .. } if cells.len() == 4)
        );
        assert_eq!(
            Frame::decode(&Frame::Grid(first.clone()).encode().unwrap())
                .unwrap()
                .unwrap()
                .0,
            Frame::Grid(first)
        );
        assert_eq!(
            encoder
                .poll(Duration::ZERO, original.clone(), Modes::default(), 3, false)
                .unwrap(),
            None
        );
        let changed = screen(&["ab", "xy"], Cursor { row: 1, col: 1 });
        let mut modes = Modes {
            screen: Screen::Alternate,
            ..Modes::default()
        };
        let diff = encoder
            .poll(Duration::from_millis(1), changed.clone(), modes, 5, false)
            .unwrap()
            .unwrap();
        assert!(
            matches!(diff, GridFrame::Diff { frame: 2, base_frame: 1, rows, cursor: Cursor { row: 1, col: 1 }, .. } if rows.len() == 1 && rows[0].row == 1)
        );
        modes.cursor_visible = false;
        let mode_only = encoder
            .poll(Duration::from_millis(2), changed, modes, 5, false)
            .unwrap()
            .unwrap();
        assert!(matches!(mode_only, GridFrame::Diff { frame: 3, rows, .. } if rows.is_empty()));
        let resized = screen(&["abc"], Cursor::default());
        assert!(matches!(
            encoder
                .poll(Duration::from_millis(3), resized, modes, 6, false)
                .unwrap(),
            Some(GridFrame::Full { frame: 4, .. })
        ));
    }

    #[test]
    fn cap_coalesces_to_latest_grid_and_rejects_clock_regression() {
        let mut encoder = GridEncoder::new(Some(2)).unwrap();
        let at = Duration::from_secs(1);
        let first = screen(&["a"], Cursor::default());
        encoder.poll(at, first, Modes::default(), 0, false).unwrap();
        assert_eq!(encoder.due(at + Duration::from_millis(499)), Ok(false));
        assert_eq!(
            encoder
                .poll(
                    at + Duration::from_millis(499),
                    screen(&["b"], Cursor::default()),
                    Modes::default(),
                    1,
                    false
                )
                .unwrap(),
            None
        );
        assert!(
            matches!(encoder.poll(at + Duration::from_millis(500), screen(&["c"], Cursor::default()), Modes::default(), 2, false).unwrap(),
            Some(GridFrame::Diff { rows, output_sequence: 2, .. }) if rows[0].cells[0].cluster == "c")
        );
        assert_eq!(
            encoder.poll(
                Duration::ZERO,
                screen(&["c"], Cursor::default()),
                Modes::default(),
                2,
                false
            ),
            Err(FrameError::InvalidValue)
        );
        assert_eq!(
            GridEncoder::new(Some(0)).err(),
            Some(FrameError::InvalidValue)
        );
    }

    #[test]
    fn hyperlinks_and_wide_cells_survive_wire_round_trip() {
        let size = Size::new(2, 1).unwrap();
        let cells = vec![
            Cell {
                text: "中".into(),
                width: CellWidth::Wide,
                hyperlink: Some("https://example.test/a".into()),
                ..Cell::blank()
            },
            Cell {
                width: CellWidth::SpacerTail,
                ..Cell::blank()
            },
        ];
        let grid = Grid::new(size, cells, Cursor::default()).unwrap();
        let mut encoder = GridEncoder::new(None).unwrap();
        let full = encoder
            .poll(Duration::ZERO, grid, Modes::default(), 0, false)
            .unwrap()
            .unwrap();
        let bytes = Frame::Grid(full).encode().unwrap();
        let (
            Frame::Grid(GridFrame::Full {
                cells, hyperlinks, ..
            }),
            _,
        ) = Frame::decode(&bytes).unwrap().unwrap()
        else {
            panic!("wrong frame")
        };
        assert_eq!(cells[0].width, CellWidth::Wide);
        assert_eq!(cells[1].width, CellWidth::SpacerTail);
        assert_eq!(cells[0].hyperlink_id, Some(1));
        assert_eq!(hyperlinks[0].uri, "https://example.test/a");
    }

    #[test]
    fn adding_a_link_keeps_ids_in_unchanged_rows() {
        let size = Size::new(1, 2).unwrap();
        let old = vec![
            Cell {
                text: "z".into(),
                hyperlink: Some("z".into()),
                ..Cell::blank()
            },
            Cell::blank(),
        ];
        let first = Grid::new(size, old.clone(), Cursor::default()).unwrap();
        let mut encoder = GridEncoder::new(None).unwrap();
        assert!(
            matches!(encoder.poll(Duration::ZERO, first, Modes::default(), 0, false).unwrap(),
            Some(GridFrame::Full { cells, .. }) if cells[0].hyperlink_id == Some(1))
        );
        let mut next = old;
        next[1].hyperlink = Some("a".into());
        let next = Grid::new(size, next, Cursor::default()).unwrap();
        let diff = encoder
            .poll(Duration::from_millis(1), next, Modes::default(), 1, false)
            .unwrap();
        assert!(
            matches!(diff, Some(GridFrame::Diff { rows, hyperlinks, .. })
            if rows.len() == 1 && rows[0].row == 1 && rows[0].cells[0].hyperlink_id == Some(2)
            && hyperlinks == [Hyperlink { id: 1, uri: "z".into() }, Hyperlink { id: 2, uri: "a".into() }])
        );
    }

    #[test]
    fn invalid_shapes_and_oversized_frames_do_not_advance_encoder() {
        let mut encoder = GridEncoder::new(None).unwrap();
        let huge = Grid::new(
            Size::new(1, 1).unwrap(),
            vec![Cell {
                text: "x".repeat(crate::channel::MAX_FRAME),
                ..Cell::blank()
            }],
            Cursor::default(),
        )
        .unwrap();
        assert_eq!(
            encoder.poll(Duration::ZERO, huge, Modes::default(), 0, false),
            Err(FrameError::TooLarge)
        );
        assert!(matches!(
            encoder
                .poll(
                    Duration::ZERO,
                    screen(&["a"], Cursor::default()),
                    Modes::default(),
                    0,
                    false
                )
                .unwrap(),
            Some(GridFrame::Full { frame: 1, .. })
        ));
        let mut bad = Frame::Grid(GridFrame::Diff {
            frame: 2,
            base_frame: 2,
            output_sequence: 0,
            size: WireSize { cols: 1, rows: 1 },
            rows: vec![],
            cursor: Cursor::default(),
            modes: Modes::default(),
            hyperlinks: vec![],
        });
        assert_eq!(bad.encode(), Err(FrameError::InvalidValue));
        if let Frame::Grid(GridFrame::Diff { base_frame, .. }) = &mut bad {
            *base_frame = 1;
        }
        let mut bytes = bad.encode().unwrap();
        let at = bytes
            .windows(14)
            .position(|window| window == b"\"base_frame\":1")
            .unwrap();
        bytes[at + 13] = b'2';
        assert_eq!(Frame::decode(&bytes), Err(FrameError::InvalidValue));
    }

    #[test]
    fn full_and_diff_reject_invalid_wire_values() {
        let mut encoder = GridEncoder::new(None).unwrap();
        let first = screen(&["ab", "cd"], Cursor::default());
        let full = encoder
            .poll(Duration::ZERO, first, Modes::default(), 0, false)
            .unwrap()
            .unwrap();
        let next = screen(&["ab", "xy"], Cursor::default());
        let diff = encoder
            .poll(Duration::from_millis(1), next, Modes::default(), 2, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            Frame::decode(&Frame::Grid(diff.clone()).encode().unwrap())
                .unwrap()
                .unwrap()
                .0,
            Frame::Grid(diff.clone())
        );
        let mut invalid = Vec::new();
        let mut zero_frame = full.clone();
        if let GridFrame::Full { frame, .. } = &mut zero_frame {
            *frame = 0;
        }
        invalid.push(zero_frame);
        let mut short = full.clone();
        if let GridFrame::Full { cells, .. } = &mut short {
            cells.pop();
        }
        invalid.push(short);
        let mut cursor = full.clone();
        if let GridFrame::Full { cursor, .. } = &mut cursor {
            cursor.col = 2;
        }
        invalid.push(cursor);
        let mut empty_cluster = full.clone();
        if let GridFrame::Full { cells, .. } = &mut empty_cluster {
            cells[0].cluster.clear();
        }
        invalid.push(empty_cluster);
        let mut duplicate_link = full.clone();
        if let GridFrame::Full { hyperlinks, .. } = &mut duplicate_link {
            hyperlinks.extend([
                Hyperlink {
                    id: 1,
                    uri: "a".into(),
                },
                Hyperlink {
                    id: 1,
                    uri: "b".into(),
                },
            ]);
        }
        invalid.push(duplicate_link);
        let mut link = full;
        if let GridFrame::Full { cells, .. } = &mut link {
            cells[0].hyperlink_id = Some(1);
        }
        invalid.push(link);
        let mut bad_row = diff.clone();
        if let GridFrame::Diff { rows, .. } = &mut bad_row {
            rows[0].row = 2;
        }
        invalid.push(bad_row);
        let mut short_row = diff.clone();
        if let GridFrame::Diff { rows, .. } = &mut short_row {
            rows[0].cells.pop();
        }
        invalid.push(short_row);
        let mut gap = diff.clone();
        if let GridFrame::Diff { frame, .. } = &mut gap {
            *frame = 4;
        }
        invalid.push(gap);
        let mut bad_base = diff;
        if let GridFrame::Diff { base_frame, .. } = &mut bad_base {
            *base_frame = 0;
        }
        invalid.push(bad_base);
        for value in invalid {
            assert_eq!(Frame::Grid(value).encode(), Err(FrameError::InvalidValue));
        }
    }

    #[test]
    fn grid_decoder_rejects_unknown_fields_and_wrong_header_version() {
        let mut encoder = GridEncoder::new(None).unwrap();
        let full = encoder
            .poll(
                Duration::ZERO,
                screen(&["a"], Cursor::default()),
                Modes::default(),
                0,
                false,
            )
            .unwrap()
            .unwrap();
        let mut bytes = Frame::Grid(full).encode().unwrap();
        bytes[4..6].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(Frame::decode(&bytes), Err(FrameError::UnsupportedVersion));
        let body: serde_json::Value = serde_json::from_slice(&bytes[7..]).unwrap();
        for field in ["extra", "cursor", "modes"] {
            let mut value = body.clone();
            if field == "extra" {
                value[field] = serde_json::json!(1);
            } else {
                value[field]["extra"] = serde_json::json!(1);
            }
            let payload = value.to_string();
            let length = u32::try_from(payload.len() + 3).unwrap();
            let mut invalid = length.to_be_bytes().to_vec();
            invalid.extend_from_slice(&crate::channel::VERSION.to_be_bytes());
            invalid.push(9);
            invalid.extend_from_slice(payload.as_bytes());
            assert_eq!(Frame::decode(&invalid), Err(FrameError::MalformedPayload));
        }
    }

    proptest! {
        #[test]
        fn one_changed_row_is_the_only_diff_row(row in 0usize..4, value in b'a'..=b'z') {
            let value = char::from(value);
            let mut encoder = GridEncoder::new(None).unwrap();
            let original = screen(&["aa", "bb", "cc", "dd"], Cursor::default());
            encoder.poll(Duration::ZERO, original.clone(), Modes::default(), 0, false).unwrap();
            let mut cells = original.cells().to_vec();
            cells[row * 2].text = value.to_string();
            let changed = Grid::new(original.size(), cells, Cursor::default()).unwrap();
            let next = encoder.poll(Duration::from_millis(1), changed, Modes::default(), 1, false).unwrap();
            if value == 'a' && row == 0 || value == 'b' && row == 1 || value == 'c' && row == 2 || value == 'd' && row == 3 {
                prop_assert_eq!(next, None);
            } else {
                let one_row = matches!(next, Some(GridFrame::Diff { rows, .. }) if rows.len() == 1 && usize::from(rows[0].row) == row);
                prop_assert!(one_row);
            }
        }
    }
}
