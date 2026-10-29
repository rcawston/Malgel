//! A table being edited in Live Preview: its cells' text, for the editor to
//! draw the table with the cell being edited in it.

use std::ops::Range;

use markdown::mdast::{AlignKind, Node};

use crate::{
    analysis::preview_parse_options,
    live::{Editable, Target, cell_range},
};

#[derive(Debug, Clone)]
pub struct Cell {
    /// The cell's text in the document.
    pub range: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct TableModel {
    pub rows: Vec<Vec<Cell>>,
    pub aligns: Vec<AlignKind>,
    /// The row and column being edited.
    pub active: (usize, usize),
}

/// The table `node` (from `source`, which starts at `offset` in the
/// document) with the cell `editable` edits.
pub fn model(node: &Node, source: &str, offset: usize, editable: &Editable) -> Option<TableModel> {
    let Target::Cell { row, col, .. } = editable.target else {
        return None;
    };
    let mut model = shown(node, source, offset)?;
    model.active = (row, col);
    Some(model)
}

/// The table `node` (from `source`, which starts at `offset` in the
/// document) with no cell being edited.
pub fn shown(node: &Node, source: &str, offset: usize) -> Option<TableModel> {
    let Node::Table(table) = node else {
        return None;
    };
    let rows = table
        .children
        .iter()
        .map(|row| {
            row.children()
                .map(|cells| {
                    cells
                        .iter()
                        .map(|cell| {
                            let range = cell_range(source, cell);
                            Cell {
                                text: source.get(range.clone()).unwrap_or_default().to_string(),
                                range: range.start + offset..range.end + offset,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    Some(TableModel {
        rows,
        aligns: table.align.clone(),
        active: NO_CELL,
    })
}

/// What [`TableModel::active`] is when no cell is being edited.
pub const NO_CELL: (usize, usize) = (usize::MAX, usize::MAX);

impl TableModel {
    /// Whether a row has nothing in any of its cells.
    pub fn has_empty_row(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.iter().all(|cell| cell.text.trim().is_empty()))
    }
}

/// The table in `source` that `editable` (a cell) is in.
pub fn model_at(source: &str, editable: &Editable) -> Option<TableModel> {
    let Target::Cell { table, .. } = &editable.target else {
        return None;
    };
    let Ok(Node::Root(root)) = markdown::to_mdast(source, &preview_parse_options()) else {
        return None;
    };
    let node = root.children.iter().find(|node| {
        node.position()
            .is_some_and(|position| position.start.offset == table.start)
    })?;
    model(node, source, 0, editable)
}
