use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const NEWLINE: u8 = b'\n';
const CARRIAGE_RETURN: u8 = b'\r';
const HEAD_BYTES: usize = 4096;
const CHUNK_BYTES: usize = 64 * 1024;

/// A skipped line keeps its first bytes as `head` so a parser can tell which record type it was.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LineEvent<'a> {
    Line(Cow<'a, str>),
    Skipped { bytes: u64, head: String },
}

pub(crate) fn for_each_line_in_file(
    path: &Path,
    max_line_bytes: u64,
    on_event: impl FnMut(LineEvent<'_>),
) -> io::Result<()> {
    for_each_line(File::open(path)?, max_line_bytes, on_event)
}

/// Lines longer than `max_line_bytes` (excluding the terminator) are reported as skipped without being buffered.
pub(crate) fn for_each_line<R: Read>(
    mut reader: R,
    max_line_bytes: u64,
    mut on_event: impl FnMut(LineEvent<'_>),
) -> io::Result<()> {
    let mut state = LineState::default();
    let mut chunk = vec![0u8; CHUNK_BYTES];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let data = &chunk[..read];
        let mut start = 0;
        while start < data.len() {
            let newline = data[start..]
                .iter()
                .position(|&b| b == NEWLINE)
                .map(|i| start + i);
            let end = newline.unwrap_or(data.len());
            state.append(&data[start..end], max_line_bytes);
            match newline {
                Some(index) => {
                    state.flush(max_line_bytes, &mut on_event);
                    start = index + 1;
                }
                None => break,
            }
        }
    }
    state.flush(max_line_bytes, &mut on_event);
    Ok(())
}

#[derive(Default)]
struct LineState {
    buffer: Vec<u8>,
    length: u64,
    last_byte: Option<u8>,
    oversized: bool,
    head: Vec<u8>,
}

impl LineState {
    fn append(&mut self, piece: &[u8], max_line_bytes: u64) {
        if let Some(&last) = piece.last() {
            self.length += piece.len() as u64;
            self.last_byte = Some(last);
        }
        // One extra byte is tolerated while streaming because it may be a CR that flush() strips.
        if self.length > max_line_bytes.saturating_add(1) {
            if !self.oversized {
                self.head = self
                    .buffer
                    .iter()
                    .chain(piece)
                    .take(HEAD_BYTES)
                    .copied()
                    .collect();
                self.oversized = true;
            }
            self.buffer.clear();
        } else if !self.oversized {
            self.buffer.extend_from_slice(piece);
        }
    }

    fn flush(&mut self, max_line_bytes: u64, on_event: &mut impl FnMut(LineEvent<'_>)) {
        let content_length = if self.last_byte == Some(CARRIAGE_RETURN) {
            self.length - 1
        } else {
            self.length
        };
        if self.oversized || content_length > max_line_bytes {
            let source = if self.oversized {
                &self.head
            } else {
                &self.buffer
            };
            let keep = source
                .len()
                .min(HEAD_BYTES)
                .min(usize::try_from(content_length).unwrap_or(usize::MAX));
            on_event(LineEvent::Skipped {
                bytes: content_length,
                head: String::from_utf8_lossy(&source[..keep]).into_owned(),
            });
        } else if content_length > 0 {
            let end = content_length as usize;
            on_event(LineEvent::Line(String::from_utf8_lossy(
                &self.buffer[..end],
            )));
        }
        self.buffer.clear();
        if self.buffer.capacity() > CHUNK_BYTES * 16 {
            self.buffer.shrink_to(CHUNK_BYTES);
        }
        self.length = 0;
        self.last_byte = None;
        self.oversized = false;
        self.head.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Owned {
        Line(String),
        Skipped(u64, String),
    }

    struct SmallChunks<'a> {
        data: &'a [u8],
        chunk: usize,
    }

    impl Read for SmallChunks<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let count = self.data.len().min(self.chunk).min(buf.len());
            buf[..count].copy_from_slice(&self.data[..count]);
            self.data = &self.data[count..];
            Ok(count)
        }
    }

    fn collect_with_chunk(content: &[u8], max: u64, chunk: usize) -> Vec<Owned> {
        let mut events = Vec::new();
        for_each_line(
            SmallChunks {
                data: content,
                chunk,
            },
            max,
            |event| {
                events.push(match event {
                    LineEvent::Line(text) => Owned::Line(text.into_owned()),
                    LineEvent::Skipped { bytes, head } => Owned::Skipped(bytes, head),
                })
            },
        )
        .unwrap();
        events
    }

    fn collect(content: &[u8], max: u64) -> Vec<Owned> {
        collect_with_chunk(content, max, CHUNK_BYTES)
    }

    fn line(text: &str) -> Owned {
        Owned::Line(text.to_string())
    }

    #[test]
    fn splits_lf_and_crlf_and_keeps_final_line_without_newline() {
        assert_eq!(
            collect(b"a\r\nb\nc", 100),
            vec![line("a"), line("b"), line("c")]
        );
    }

    #[test]
    fn reports_oversized_line_and_resumes() {
        let content = format!("ok\n{}\nafter\n", "x".repeat(50));
        assert_eq!(
            collect(content.as_bytes(), 10),
            vec![
                line("ok"),
                Owned::Skipped(50, "x".repeat(50)),
                line("after")
            ]
        );
    }

    #[test]
    fn size_limit_excludes_crlf_terminator() {
        assert_eq!(
            collect(b"abc\r\nabcd\r\nab\r\n", 3),
            vec![line("abc"), Owned::Skipped(4, "abcd".into()), line("ab")]
        );
    }

    #[test]
    fn crlf_split_across_chunks_is_stripped() {
        assert_eq!(
            collect_with_chunk(b"abc\r\nde\r\n", 3, 4),
            vec![line("abc"), line("de")]
        );
    }

    #[test]
    fn skipped_line_keeps_only_first_4k_head_across_chunks() {
        let text = format!("{{\"type\":\"event_msg\"}}{}", "z".repeat(300_000));
        let content = format!("{text}\nnext\n");
        let events = collect_with_chunk(content.as_bytes(), 100_000, 1000);
        assert_eq!(
            events,
            vec![
                Owned::Skipped(text.len() as u64, text[..4096].to_string()),
                line("next")
            ]
        );
    }

    #[test]
    fn line_spanning_many_chunks() {
        let long = "y".repeat(200_000);
        let content = format!("{long}\nend\n");
        assert_eq!(
            collect_with_chunk(content.as_bytes(), 1_000_000, 4096),
            vec![line(&long), line("end")]
        );
    }

    #[test]
    fn multibyte_utf8_split_across_chunks() {
        let text = "測試".repeat(40_000);
        let content = format!("{text}\n");
        assert_eq!(
            collect_with_chunk(content.as_bytes(), 10_000_000, 4097),
            vec![line(&text)]
        );
    }

    #[test]
    fn skips_empty_lines() {
        assert_eq!(collect(b"\n\na\n\r\n\n", 100), vec![line("a")]);
    }
}
