//! Stable source-metadata ordering without an unbounded sort call.

pub(in crate::editor) struct MetadataOrder {
    indices: Vec<usize>,
    spare: Vec<usize>,
    phase: Phase,
    width: usize,
    start: usize,
    left: usize,
    right: usize,
    output: usize,
    position: usize,
    insertion_steps: usize,
}

enum Phase {
    Check,
    Insertion,
    Initialize,
    Merge,
    Copy,
    Invert,
    Reorder,
    Complete,
}

impl MetadataOrder {
    pub fn new() -> Self {
        Self {
            indices: Vec::new(),
            spare: Vec::new(),
            phase: Phase::Check,
            width: 1,
            start: 0,
            left: 0,
            right: 1,
            output: 0,
            position: 1,
            insertion_steps: 0,
        }
    }

    fn next_run(&mut self, end: usize, length: usize) {
        self.phase = Phase::Merge;
        if end < length {
            self.start = end;
        } else {
            self.width = self.width.saturating_mul(2);
            self.start = 0;
            if self.width >= length {
                self.phase = Phase::Invert;
                self.position = 0;
            }
        }
        self.left = self.start;
        self.right = (self.start + self.width).min(length);
        self.output = self.start;
    }

    /// One bounded unit: initialize, merge or move at most one record.
    pub fn step<T>(&mut self, values: &mut [T], key: impl Fn(&T) -> (usize, usize)) -> bool {
        let length = values.len();
        match self.phase {
            Phase::Check => {
                if self.position >= length {
                    self.phase = Phase::Complete;
                } else if key(&values[self.position - 1]) <= key(&values[self.position]) {
                    self.position += 1;
                } else {
                    self.left = self.position;
                    self.phase = Phase::Insertion;
                }
            }
            Phase::Insertion => {
                // Parser metadata is usually ordered except for enclosing ranges.
                // Bound this adaptive path so adversarial order cannot become quadratic.
                let complete =
                    if self.left > 0 && key(&values[self.left - 1]) > key(&values[self.left]) {
                        values.swap(self.left - 1, self.left);
                        self.left -= 1;
                        self.left == 0 || key(&values[self.left - 1]) <= key(&values[self.left])
                    } else {
                        true
                    };
                if complete {
                    self.position += 1;
                    self.left = self.position;
                    if self.position == length {
                        self.phase = Phase::Complete;
                    }
                }
                self.insertion_steps += 1;
                if self.insertion_steps >= length.saturating_mul(8)
                    && !matches!(self.phase, Phase::Complete)
                {
                    self.indices.reserve(length);
                    self.spare.reserve(length);
                    self.position = 0;
                    self.left = 0;
                    self.right = 1;
                    self.phase = Phase::Initialize;
                }
            }
            Phase::Initialize => {
                if self.indices.len() < length {
                    self.indices.push(self.indices.len());
                    self.spare.push(0);
                } else {
                    self.phase = if length <= 1 {
                        Phase::Complete
                    } else {
                        Phase::Merge
                    };
                }
            }
            Phase::Merge => {
                let middle = (self.start + self.width).min(length);
                let end = (middle + self.width).min(length);
                if self.output == self.start
                    && (middle == end
                        || key(&values[self.indices[middle - 1]])
                            <= key(&values[self.indices[middle]]))
                {
                    self.next_run(end, length);
                } else if self.output < end {
                    // Left-first equality preserves the previous stable ordering.
                    let take_left = self.left < middle
                        && (self.right >= end
                            || key(&values[self.indices[self.left]])
                                <= key(&values[self.indices[self.right]]));
                    let input = if take_left {
                        &mut self.left
                    } else {
                        &mut self.right
                    };
                    self.spare[self.output] = self.indices[*input];
                    *input += 1;
                    self.output += 1;
                } else {
                    self.position = self.start;
                    self.phase = Phase::Copy;
                }
            }
            Phase::Copy => {
                if self.position < self.output {
                    self.indices[self.position] = self.spare[self.position];
                    self.position += 1;
                } else {
                    self.next_run(self.output, length);
                }
            }
            Phase::Invert => {
                if self.position < length {
                    self.spare[self.indices[self.position]] = self.position;
                    self.position += 1;
                } else {
                    self.position = 0;
                    self.phase = Phase::Reorder;
                }
            }
            Phase::Reorder => {
                if self.position == length {
                    self.phase = Phase::Complete;
                } else if self.spare[self.position] == self.position {
                    self.position += 1;
                } else {
                    let target = self.spare[self.position];
                    values.swap(self.position, target);
                    self.spare.swap(self.position, target);
                }
            }
            Phase::Complete => {}
        }
        matches!(self.phase, Phase::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataOrder;

    #[test]
    fn bounded_order_matches_stable_sort_including_equal_keys() {
        for length in 0..130 {
            for seed in 0..7 {
                let mut values: Vec<_> = (0..length)
                    .map(|index| ((index * 37 + seed * 11) % 13, index))
                    .collect();
                let mut expected = values.clone();
                expected.sort_by_key(|value| value.0);
                let mut ordering = MetadataOrder::new();
                let mut steps = 0;
                while !ordering.step(&mut values, |value| (value.0, 0)) {
                    steps += 1;
                    assert!(steps < 50 * (length + 1));
                }
                assert_eq!(values, expected);
            }
        }
    }

    #[test]
    fn nearly_ordered_enclosing_ranges_use_linear_work_without_scratch_tables() {
        let mut values = vec![(0, 80_000), (0, 79_999)];
        for position in 0..4000 {
            values.extend([
                (position * 20, 19),
                (position * 20, 7),
                (position * 20 + 2, 3),
            ]);
        }
        let mut expected = values.clone();
        expected.sort_by_key(|value| *value);
        let mut order = MetadataOrder::new();
        let mut steps = 0;
        while !order.step(&mut values, |value| *value) {
            steps += 1;
            assert!(steps < 8 * values.len());
        }
        assert_eq!(values, expected);
        assert!(order.indices.is_empty());
        assert!(order.spare.is_empty());
    }

    #[test]
    fn reversed_order_uses_bounded_fallback_and_each_step_has_constant_comparisons() {
        let mut values: Vec<_> = (0..4096).rev().map(|index| (index / 3, index)).collect();
        let mut expected = values.clone();
        expected.sort_by_key(|value| value.0);
        let mut order = MetadataOrder::new();
        loop {
            let comparisons = std::cell::Cell::new(0);
            let complete = order.step(&mut values, |value| {
                comparisons.set(comparisons.get() + 1);
                (value.0, 0)
            });
            assert!(comparisons.get() <= 4);
            if complete {
                break;
            }
        }
        assert_eq!(values, expected);
        assert_eq!(order.indices.len(), values.len());
    }
}
