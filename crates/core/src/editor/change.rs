//! Resumable exact replacement boundaries for source-owned preparation jobs.
use super::TextChange;

/// Callers retain the same two immutable sources until completion or discard
/// this job on source replacement. No borrowed source escapes an advance.
#[derive(Default)]
pub struct TextChangePreparation {
    prefix: usize,
    suffix: usize,
    prefix_complete: bool,
    complete: bool,
    change: Option<TextChange>,
}

impl TextChangePreparation {
    pub const fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn change(&self) -> Option<&TextChange> {
        self.change.as_ref()
    }

    /// Charge every raw-byte comparison to the budget, then round the finished
    /// boundaries to UTF-8 scalar positions. Zero budgets never advance.
    pub fn advance(&mut self, old: &str, new: &str, budget: usize) -> usize {
        if self.complete || budget == 0 {
            return 0;
        }
        let mut remaining = budget;
        if !self.prefix_complete {
            let limit = old.len().min(new.len());
            let end = limit.min(self.prefix.saturating_add(remaining));
            let equal = old.as_bytes()[self.prefix..end]
                .iter()
                .zip(&new.as_bytes()[self.prefix..end])
                .take_while(|(a, b)| a == b)
                .count();
            remaining = remaining.saturating_sub(equal + usize::from(equal < end - self.prefix));
            self.prefix += equal;
            if self.prefix < end || end == limit {
                self.prefix_complete = true;
                while !old.is_char_boundary(self.prefix) || !new.is_char_boundary(self.prefix) {
                    self.prefix -= 1;
                }
                if self.prefix == old.len() && old.len() == new.len() {
                    self.complete = true;
                    return budget - remaining;
                }
            }
        }
        if self.prefix_complete {
            let limit = old.len().min(new.len()) - self.prefix;
            let end = limit.min(self.suffix.saturating_add(remaining));
            let equal = old.as_bytes()[old.len() - end..old.len() - self.suffix]
                .iter()
                .rev()
                .zip(
                    new.as_bytes()[new.len() - end..new.len() - self.suffix]
                        .iter()
                        .rev(),
                )
                .take_while(|(a, b)| a == b)
                .count();
            remaining = remaining.saturating_sub(equal + usize::from(equal < end - self.suffix));
            self.suffix += equal;
            if self.suffix < end || end == limit {
                while !old.is_char_boundary(old.len() - self.suffix)
                    || !new.is_char_boundary(new.len() - self.suffix)
                {
                    self.suffix -= 1;
                }
                self.change = Some(TextChange {
                    range: self.prefix..old.len() - self.suffix,
                    new_end: new.len() - self.suffix,
                });
                self.complete = true;
            }
        }
        budget - remaining
    }
}
