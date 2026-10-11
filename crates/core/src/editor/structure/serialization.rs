//! Incremental wire encoding of fixed-size structural records.
use super::{StructuralRecords, StructurePublication};
use crate::editor::syntax::json::{self, JsonArray};
use serde::Serialize;

#[derive(Default)]
pub(in crate::editor) struct StructureSerialization {
    phase: usize,
    started: bool,
    array: JsonArray,
}
impl StructureSerialization {
    pub fn advance(&mut self, output: &mut String, publication: &StructurePublication) -> bool {
        if !self.started {
            output.push_str(match publication {
                StructurePublication::Full(_) => "{\"language\":",
                StructurePublication::Changes { .. } => "{\"changes\":{\"language\":",
            });
            json::value(
                output,
                &match publication {
                    StructurePublication::Full(data) => data.language,
                    StructurePublication::Changes { changes } => changes.language,
                },
            );
            self.started = true;
        }
        let complete = match publication {
            StructurePublication::Full(data) => match self.phase {
                0 => self.full(output, "scopes", &data.scopes),
                1 => self.full(output, "selections", &data.selections),
                2 => self.full(output, "opaque_starts", &data.opaque_starts),
                3 => self.full(output, "protected", &data.protected),
                4 => self.full(output, "brackets", &data.brackets),
                _ => return true,
            },
            StructurePublication::Changes { changes } => match self.phase {
                0 => self.records(output, "scopes", &changes.scopes),
                1 => self.records(output, "selections", &changes.selections),
                2 => self.records(output, "opaque_starts", &changes.opaque_starts),
                3 => self.records(output, "protected", &changes.protected),
                4 => self.records(output, "brackets", &changes.brackets),
                _ => return true,
            },
        };
        if complete {
            self.phase += 1;
            self.array = JsonArray::default();
            if self.phase == 5 {
                output.push('}');
                if matches!(publication, StructurePublication::Changes { .. }) {
                    output.push('}');
                }
                return true;
            }
        }
        false
    }
    fn header(output: &mut String, name: &str) {
        output.push(',');
        json::value(output, name);
        output.push(':');
    }
    fn full<T: Serialize>(&mut self, output: &mut String, name: &str, values: &[T]) -> bool {
        if self.array.is_new() {
            Self::header(output, name);
        }
        self.array.advance(output, values)
    }
    fn records<T: Serialize>(
        &mut self,
        output: &mut String,
        name: &str,
        records: &StructuralRecords<T>,
    ) -> bool {
        if self.array.is_new() {
            Self::header(output, name);
            if let StructuralRecords::Replace { start, end, .. } = records {
                output.push_str("{\"start\":");
                json::value(output, start);
                output.push_str(",\"end\":");
                json::value(output, end);
                output.push_str(",\"items\":");
            }
        }
        let items = match records {
            StructuralRecords::Full(items) | StructuralRecords::Replace { items, .. } => items,
        };
        let done = self.array.advance(output, items);
        if done && matches!(records, StructuralRecords::Replace { .. }) {
            output.push('}');
        }
        done
    }
}
