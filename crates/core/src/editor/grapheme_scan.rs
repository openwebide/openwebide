//! Bounded Unicode source/context traversal shared by editor preparations.
use std::ops::Range;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

pub(super) struct GraphemeStep {
    pub consumed: Range<usize>,
    pub boundary: Option<usize>,
}

pub(super) struct GraphemeScan<'a> {
    text: &'a str,
    cursor: GraphemeCursor,
    chunk: Range<usize>,
    progress: usize,
    done: bool,
}
impl<'a> GraphemeScan<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            cursor: GraphemeCursor::new(0, text.len(), true),
            chunk: 0..text.floor_char_boundary(512.min(text.len())),
            progress: 0,
            done: false,
        }
    }
    pub fn progress(&self) -> usize {
        self.progress
    }
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// One bounded scan unit. A consumer can stop at an exact boundary earlier.
    /// Forward/context progress and cursor calls both have fixed limits; context
    /// requests need not move the forward cursor. Each supplied chunk is <=512 bytes.
    pub fn advance(&mut self, mut consume: impl FnMut(GraphemeStep) -> bool) {
        let before = self.progress;
        for _ in 0..512 {
            if self.done || self.progress - before >= 512 {
                break;
            }
            let offset = self.cursor.cur_cursor();
            let result = self
                .cursor
                .next_boundary(&self.text[self.chunk.clone()], self.chunk.start);
            let end = self.cursor.cur_cursor();
            self.progress += end - offset;
            let boundary = match result {
                Ok(boundary) => {
                    self.done = boundary.is_none() || boundary == Some(self.text.len());
                    boundary
                }
                Err(GraphemeIncomplete::NextChunk) => {
                    // Preserve the preceding scalar. At an exact chunk start
                    // regional context would otherwise count known parity twice.
                    let start = self
                        .text
                        .floor_char_boundary(self.chunk.end.saturating_sub(4));
                    self.chunk = start
                        ..self
                            .text
                            .floor_char_boundary((start + 512).min(self.text.len()));
                    None
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let start = self.text.ceil_char_boundary(end.saturating_sub(512));
                    self.cursor.provide_context(&self.text[start..end], start);
                    self.progress += end - start;
                    None
                }
                Err(GraphemeIncomplete::PrevChunk | GraphemeIncomplete::InvalidOffset) => {
                    unreachable!("forward cursor always retains its exact source chunk");
                }
            };
            if consume(GraphemeStep {
                consumed: offset..end,
                boundary,
            }) {
                break;
            }
        }
    }
}
