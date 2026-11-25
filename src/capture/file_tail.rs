//! Incremental log-file tail for the no-agent capture path (T4-5).
//!
//! The platform cannot read a run console, so when no Java agent is attached the
//! user exports the log to a file and the plugin follows it. `host::fs::read_file`
//! (and `Project::read_file`) return the whole text; this module keeps the byte
//! offset and slices out only what appeared since the previous poll.
//!
//! Parsing is deliberately out of scope: lane L4 owns the log grammars, which
//! are being built in parallel. The caller injects its parser through
//! [`LineSink`], so this module only decides where a line starts and ends.
//!
//! Invariants:
//!
//! - the committed offset always sits on a line boundary (right after `\n`),
//!   so slicing never splits a UTF-8 character;
//! - an incomplete trailing line is not emitted until its `\n` arrives, and the
//!   offset does not move past its start;
//! - a file that shrank (rotation/truncation) restarts at offset 0 and line 1,
//!   and the chunk says so with `reset = true`.

use openidea_plugin::Error;
use openidea_plugin::host::{self, Project};

/// Receives complete lines. The caller injects the L4 parser here.
pub trait LineSink {
    fn push_line(&mut self, line_no: u32, line: &str);
}

/// Byte offset plus the next 1-based line number. Cheap to persist between polls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailState {
    pub offset: u64,
    pub next_line: u32,
}

impl Default for TailState {
    fn default() -> Self {
        Self::new()
    }
}

impl TailState {
    pub fn new() -> Self {
        Self {
            offset: 0,
            next_line: 1,
        }
    }

    pub fn at(offset: u64, next_line: u32) -> Self {
        Self {
            offset,
            next_line: next_line.max(1),
        }
    }
}

/// One incremental read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TailChunk {
    /// The new complete lines as text (exactly the bytes the offset advanced by).
    pub appended: String,
    /// Where the next read starts.
    pub next_offset: u64,
    /// Complete lines handed to the sink in this call.
    pub lines_emitted: u32,
    /// The file shrank since the last read; reading restarted at 0.
    pub reset: bool,
    /// Bytes of an incomplete trailing line, kept for the next read.
    pub pending_bytes: usize,
}

impl TailState {
    /// Slices `whole` from the committed offset, emits complete lines to `sink`,
    /// and advances the state.
    ///
    /// Complete lines are `\n`-terminated; a trailing `\r` (CRLF) is stripped
    /// before the line reaches the sink, because log grammars (L4) do not expect
    /// it. Bytes after the last `\n` stay pending.
    pub fn read_increment(&mut self, whole: &str, sink: &mut dyn LineSink) -> TailChunk {
        let mut reset = false;
        let mut start = self.offset as usize;
        if start > whole.len() {
            // Rotation or truncation: the committed offset is past EOF.
            start = 0;
            self.offset = 0;
            self.next_line = 1;
            reset = true;
        }
        while start < whole.len() && !whole.is_char_boundary(start) {
            // Defensive only: our offsets always land after `\n`. Snapping forward
            // is safer than panicking on a hand-edited state.
            start += 1;
        }

        let bytes = whole.as_bytes();
        let mut line_start = start;
        let mut line_no = self.next_line;
        let mut lines_emitted = 0u32;
        let mut index = start;
        while index < bytes.len() {
            if bytes[index] == b'\n' {
                let mut line = &whole[line_start..index];
                if let Some(stripped) = line.strip_suffix('\r') {
                    line = stripped;
                }
                sink.push_line(line_no, line);
                line_no += 1;
                lines_emitted += 1;
                index += 1;
                line_start = index;
            } else {
                index += 1;
            }
        }

        self.offset = line_start as u64;
        self.next_line = line_no;
        TailChunk {
            appended: whole[start..line_start].to_owned(),
            next_offset: self.offset,
            lines_emitted,
            reset,
            pending_bytes: whole.len() - line_start,
        }
    }
}

/// Follows a file inside an open project (`project.read_file`, `project.read`).
pub fn tail_project_file(
    project: &Project,
    path: &str,
    state: &mut TailState,
    sink: &mut dyn LineSink,
) -> Result<TailChunk, Error> {
    let whole = project.read_file(path)?;
    Ok(state.read_increment(&whole, sink))
}

/// Follows a file outside the projects (an exported run log). Needs the
/// `fs.read:<path>` grant; without it the host returns `Error::Denied`.
pub fn tail_external_file(
    path: &str,
    state: &mut TailState,
    sink: &mut dyn LineSink,
) -> Result<TailChunk, Error> {
    let whole = host::fs::read_file(path)?;
    Ok(state.read_increment(&whole, sink))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        lines: Vec<(u32, String)>,
    }

    impl LineSink for Recorder {
        fn push_line(&mut self, line_no: u32, line: &str) {
            self.lines.push((line_no, line.to_owned()));
        }
    }

    #[test]
    fn first_read_emits_every_complete_line() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        let chunk = state.read_increment("one\ntwo\n", &mut sink);
        assert_eq!(
            sink.lines,
            vec![(1, "one".to_owned()), (2, "two".to_owned())]
        );
        assert_eq!(chunk.appended, "one\ntwo\n");
        assert_eq!(chunk.next_offset, 8);
        assert_eq!(chunk.lines_emitted, 2);
        assert!(!chunk.reset);
        assert_eq!(chunk.pending_bytes, 0);
        assert_eq!(state, TailState::at(8, 3));
    }

    #[test]
    fn an_appended_line_is_the_only_new_line() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        state.read_increment("one\ntwo\n", &mut sink);
        let chunk = state.read_increment("one\ntwo\nthree\n", &mut sink);
        assert_eq!(
            sink.lines,
            vec![
                (1, "one".to_owned()),
                (2, "two".to_owned()),
                (3, "three".to_owned())
            ]
        );
        assert_eq!(chunk.appended, "three\n");
        assert_eq!(chunk.next_offset, 14);
        assert_eq!(chunk.lines_emitted, 1);
        assert_eq!(state.next_line, 4);
    }

    #[test]
    fn no_new_bytes_emit_nothing_and_keep_the_offset() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        state.read_increment("line\n", &mut sink);
        let chunk = state.read_increment("line\n", &mut sink);
        assert_eq!(sink.lines.len(), 1);
        assert_eq!(chunk.appended, "");
        assert_eq!(chunk.lines_emitted, 0);
        assert_eq!(chunk.next_offset, 5);
    }

    #[test]
    fn an_incomplete_trailing_line_waits_for_its_newline() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        let first = state.read_increment("a\npart", &mut sink);
        assert_eq!(sink.lines, vec![(1, "a".to_owned())]);
        assert_eq!(first.next_offset, 2);
        assert_eq!(first.pending_bytes, 4);
        assert_eq!(state.offset, 2);

        let second = state.read_increment("a\npartial\n", &mut sink);
        assert_eq!(
            sink.lines,
            vec![(1, "a".to_owned()), (2, "partial".to_owned())]
        );
        assert_eq!(second.appended, "partial\n");
        assert_eq!(second.next_offset, 10);
        assert_eq!(second.pending_bytes, 0);
    }

    #[test]
    fn crlf_lines_reach_the_sink_without_the_carriage_return() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        let chunk = state.read_increment("a\r\nb\r\n", &mut sink);
        assert_eq!(sink.lines, vec![(1, "a".to_owned()), (2, "b".to_owned())]);
        assert_eq!(chunk.next_offset, 6);
    }

    #[test]
    fn a_truncated_file_restarts_at_zero_and_says_so() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        state.read_increment("first\nsecond\n", &mut sink);
        let chunk = state.read_increment("new\n", &mut sink);
        assert!(chunk.reset);
        assert_eq!(chunk.next_offset, 4);
        assert_eq!(state, TailState::at(4, 2));
        assert_eq!(sink.lines.last(), Some(&(1, "new".to_owned())));
    }

    #[test]
    fn multibyte_characters_are_not_split() {
        let mut state = TailState::new();
        let mut sink = Recorder::default();
        let chunk = state.read_increment("查询\n", &mut sink);
        assert_eq!(sink.lines, vec![(1, "查询".to_owned())]);
        assert_eq!(chunk.next_offset, "查询\n".len() as u64);
        assert_eq!(state.offset, chunk.next_offset);
    }

    #[test]
    fn an_offset_snapped_into_a_character_moves_forward() {
        let mut state = TailState::at(1, 1); // mid-character on purpose
        let mut sink = Recorder::default();
        state.read_increment("查询\n", &mut sink);
        // Forward snapping drops the half character; it never panics and never
        // re-emits a line already handed to the sink.
        assert_eq!(sink.lines, vec![(1, "询".to_owned())]);
        assert_eq!(state.offset, "查询\n".len() as u64);
    }
}
