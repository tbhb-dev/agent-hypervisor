//! Corpus loading and grid normalization, shared by the test targets.

use std::path::PathBuf;

use hypervisor_core::emulator::{Attrs, Cell, Cursor, Grid, Screen, Size};
use serde_json::Value;

/// One recorded session from `tests/fixtures/corpus/` and the final screen recorded for it.
pub struct Fixture {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub size: Size,
    pub cursor: Cursor,
    pub text: Vec<String>,
    pub screen: Screen,
    pub alacritty_grid_diffs: usize,
}

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/corpus")
}

/// Every recording in the corpus, in name order.
pub fn fixtures() -> Vec<Fixture> {
    let mut names: Vec<String> = std::fs::read_dir(corpus().join("expected"))
        .expect("corpus expected/")
        .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .collect();
    names.sort();
    names.iter().map(|n| fixture(n)).collect()
}

/// Loads `expected/<name>.json` and the recording it names.
pub fn fixture(name: &str) -> Fixture {
    let dir = corpus();
    let expected: Value = serde_json::from_slice(
        &std::fs::read(dir.join(format!("expected/{name}.json"))).expect("expected screen"),
    )
    .expect("expected screen JSON");
    let bytes = std::fs::read(dir.join(expected["file"].as_str().expect("file"))).expect("file");
    let num = |k: &str| expected[k].as_u64().expect(k);
    let u16_of = |v: u64| u16::try_from(v).expect("fits u16");
    assert_eq!(num("bytes"), bytes.len() as u64, "{name}: recording length");
    let cursor = expected["cursor"].as_array().expect("cursor");
    Fixture {
        name: name.to_owned(),
        size: Size::new(u16_of(num("cols")), u16_of(num("rows"))).expect("size"),
        cursor: Cursor {
            row: u16_of(cursor[0].as_u64().expect("row")),
            col: u16_of(cursor[1].as_u64().expect("col")),
        },
        text: expected["text"]
            .as_array()
            .expect("text")
            .iter()
            .map(|l| l.as_str().expect("line").to_owned())
            .collect(),
        screen: match expected["screen"].as_str() {
            Some("alternate") => Screen::Alternate,
            Some("primary") => Screen::Primary,
            other => panic!("{name}: screen {other:?}"),
        },
        alacritty_grid_diffs: usize::try_from(num("alacritty_grid_diffs")).expect("count"),
        sha256: expected["sha256"].as_str().expect("sha256").to_owned(),
        bytes,
    }
}

/// `grid` without the attributes `alacritty_terminal` has no flag for, blink and overline.
pub fn comparable(grid: &Grid) -> Grid {
    let cells = grid
        .cells()
        .iter()
        .map(|c| Cell {
            attrs: c.attrs.without(Attrs::BLINK.with(Attrs::OVERLINE)),
            ..c.clone()
        })
        .collect();
    Grid::new(grid.size(), cells, grid.cursor()).expect("same shape")
}
