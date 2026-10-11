//! Typed structural fragments; every record payload is bounded before serde use.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListHeader {
    count: usize,
    patch: Option<Range<usize>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    language: Language,
    changes: bool,
    lists: [ListHeader; 5],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::editor) struct StructureFrame {
    header: Option<Header>,
    field: Option<usize>,
    records: Option<serde_json::Value>,
}
fn header<T>(records: &StructuralRecords<T>) -> ListHeader {
    match records {
        StructuralRecords::Full(items) => ListHeader {
            count: items.len(),
            patch: None,
        },
        StructuralRecords::Replace { start, end, items } => ListHeader {
            count: items.len(),
            patch: Some(*start..*end),
        },
    }
}
fn fragment<T: Serialize>(items: &[T], offset: &mut usize) -> serde_json::Value {
    let end = items.len().min(*offset + 64);
    let value = serde_json::to_value(&items[*offset..end]).expect("structural records serialize");
    *offset = end;
    value
}
fn items<T>(records: &StructuralRecords<T>) -> &[T] {
    match records {
        StructuralRecords::Full(items) | StructuralRecords::Replace { items, .. } => items,
    }
}
pub(in crate::editor) struct StructureFrames {
    data: StructurePublication,
    field: usize,
    offset: usize,
    header: bool,
}
impl StructureFrames {
    pub fn new(data: StructurePublication) -> Self {
        Self {
            data,
            field: 0,
            offset: 0,
            header: false,
        }
    }
    pub fn next(&mut self) -> Option<StructureFrame> {
        if !self.header {
            self.header = true;
            let metadata = match &self.data {
                StructurePublication::Full(data) => Header {
                    language: data.language,
                    changes: false,
                    lists: [
                        data.scopes.len(),
                        data.selections.len(),
                        data.opaque_starts.len(),
                        data.protected.len(),
                        data.brackets.len(),
                    ]
                    .map(|count| ListHeader { count, patch: None }),
                },
                StructurePublication::Changes { changes } => Header {
                    language: changes.language,
                    changes: true,
                    lists: [
                        header(&changes.scopes),
                        header(&changes.selections),
                        header(&changes.opaque_starts),
                        header(&changes.protected),
                        header(&changes.brackets),
                    ],
                },
            };
            return Some(StructureFrame {
                header: Some(metadata),
                field: None,
                records: None,
            });
        }
        while self.field < 5 {
            macro_rules! part {
                ($data:expr) => {{
                    let list = $data;
                    if self.offset == list.len() {
                        self.field += 1;
                        self.offset = 0;
                        continue;
                    }
                    fragment(list, &mut self.offset)
                }};
            }
            let records = match &self.data {
                StructurePublication::Full(data) => match self.field {
                    0 => part!(data.scopes.as_slice()),
                    1 => part!(data.selections.as_slice()),
                    2 => part!(data.opaque_starts.as_slice()),
                    3 => part!(data.protected.as_slice()),
                    _ => part!(data.brackets.as_slice()),
                },
                StructurePublication::Changes { changes } => match self.field {
                    0 => part!(items(&changes.scopes)),
                    1 => part!(items(&changes.selections)),
                    2 => part!(items(&changes.opaque_starts)),
                    3 => part!(items(&changes.protected)),
                    _ => part!(items(&changes.brackets)),
                },
            };
            return Some(StructureFrame {
                header: None,
                field: Some(self.field),
                records: Some(records),
            });
        }
        None
    }
}
#[derive(Default)]
pub(in crate::editor) struct StructureFrameDecoding {
    header: Option<Header>,
    field: usize,
    scopes: Vec<(Range<usize>, Language)>,
    selections: Vec<Range<usize>>,
    opaque_starts: Vec<usize>,
    protected: Vec<(Range<usize>, bool, RegionKind)>,
    brackets: Vec<(usize, char, Option<usize>)>,
}
fn append<T: serde::de::DeserializeOwned>(
    target: &mut Vec<T>,
    value: serde_json::Value,
    count: usize,
) -> Option<()> {
    let records: Vec<T> = serde_json::from_value(value).ok()?;
    if records.is_empty() || records.len() > 64 || target.len().checked_add(records.len())? > count
    {
        return None;
    }
    target.extend(records);
    Some(())
}
fn restore<T>(items: Vec<T>, header: &ListHeader) -> StructuralRecords<T> {
    match &header.patch {
        Some(range) => StructuralRecords::Replace {
            start: range.start,
            end: range.end,
            items,
        },
        None => StructuralRecords::Full(items),
    }
}
impl StructureFrameDecoding {
    pub fn push(&mut self, part: StructureFrame, budget: usize) -> Option<()> {
        if let Some(header) = part.header {
            if self.header.is_some()
                || part.field.is_some()
                || part.records.is_some()
                || header
                    .lists
                    .iter()
                    .try_fold(0_usize, |sum, list| sum.checked_add(list.count))?
                    > budget
                || (!header.changes && header.lists.iter().any(|list| list.patch.is_some()))
            {
                return None;
            }
            self.header = Some(header);
            return Some(());
        }
        let header = self.header.as_ref()?;
        let lengths = [
            self.scopes.len(),
            self.selections.len(),
            self.opaque_starts.len(),
            self.protected.len(),
            self.brackets.len(),
        ];
        while self.field < 5 && lengths[self.field] == header.lists[self.field].count {
            self.field += 1;
        }
        if self.field >= 5 || part.field? != self.field {
            return None;
        }
        let value = part.records?;
        let count = header.lists[self.field].count;
        match self.field {
            0 => append(&mut self.scopes, value, count),
            1 => append(&mut self.selections, value, count),
            2 => append(&mut self.opaque_starts, value, count),
            3 => append(&mut self.protected, value, count),
            _ => append(&mut self.brackets, value, count),
        }
    }
    pub fn record_count(&self) -> Option<usize> {
        self.header
            .as_ref()?
            .lists
            .iter()
            .try_fold(0_usize, |sum, list| sum.checked_add(list.count))
    }
    pub fn complete(&self) -> bool {
        self.header.as_ref().is_some_and(|header| {
            [
                self.scopes.len(),
                self.selections.len(),
                self.opaque_starts.len(),
                self.protected.len(),
                self.brackets.len(),
            ]
            .iter()
            .zip(&header.lists)
            .all(|(len, list)| *len == list.count)
        })
    }
    pub fn finish(self) -> Option<StructurePublication> {
        if !self.complete() {
            return None;
        }
        let header = self.header?;
        Some(if header.changes {
            StructurePublication::Changes {
                changes: StructureChanges {
                    language: header.language,
                    scopes: restore(self.scopes, &header.lists[0]),
                    selections: restore(self.selections, &header.lists[1]),
                    opaque_starts: restore(self.opaque_starts, &header.lists[2]),
                    protected: restore(self.protected, &header.lists[3]),
                    brackets: restore(self.brackets, &header.lists[4]),
                },
            }
        } else {
            StructurePublication::Full(StructureData {
                language: header.language,
                scopes: self.scopes,
                selections: self.selections,
                opaque_starts: self.opaque_starts,
                protected: self.protected,
                brackets: self.brackets,
            })
        })
    }
}
