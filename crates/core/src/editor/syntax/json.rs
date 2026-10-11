//! Bounded JSON text/record writes; source and domain validation stay above this.
use serde::Serialize;

pub(in crate::editor) fn value<T: Serialize + ?Sized>(output: &mut String, value: &T) {
    output.push_str(&serde_json::to_string(value).expect("syntax wire values are serializable"));
}

pub(in crate::editor) fn text(
    output: &mut String,
    source: &str,
    offset: &mut usize,
    budget: usize,
) -> bool {
    let end = source.floor_char_boundary(source.len().min(offset.saturating_add(budget)));
    let escaped =
        serde_json::to_string(&source[*offset..end]).expect("source text is serializable");
    output.push_str(&escaped[1..escaped.len() - 1]);
    *offset = end;
    end == source.len()
}

#[derive(Default)]
pub(in crate::editor) struct JsonArray {
    offset: usize,
    started: bool,
}
impl JsonArray {
    pub fn is_new(&self) -> bool {
        !self.started
    }
    /// Fixed-size records only; large strings and nested arrays use their own cursors.
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn advance<T: Serialize>(&mut self, output: &mut String, values: &[T]) -> bool {
        self.advance_budget(output, values, 64)
    }
    pub fn advance_budget<T: Serialize>(
        &mut self,
        output: &mut String,
        values: &[T],
        budget: usize,
    ) -> bool {
        if !self.started {
            output.push('[');
            self.started = true;
        }
        let end = self.offset.saturating_add(budget).min(values.len());
        if end > self.offset {
            if self.offset > 0 {
                output.push(',');
            }
            let part = serde_json::to_string(&values[self.offset..end])
                .expect("syntax records are serializable");
            output.push_str(&part[1..part.len() - 1]);
            self.offset = end;
        }
        if end == values.len() {
            output.push(']');
            true
        } else {
            false
        }
    }
}
