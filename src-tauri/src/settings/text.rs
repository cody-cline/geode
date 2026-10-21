//! The one splicer every format writes through.
//!
//! An edit to a settings file is a **byte span** inside the text the file
//! already holds: the value of one setting is replaced, one line is taken out,
//! or one line is put in. Everything else — the comments, the blank lines, the
//! order, each line's own spacing, the file's line endings and whether it ends
//! with a newline — is copied through untouched, which is a property of
//! working this way rather than something each format has to remember.
//!
//! That matters more than it sounds: a writer that normalises line endings or
//! adds a final newline rewrites every line of the file as far as any later
//! diff or hash can tell, and 544 of the 753 real TOML configs measured on one
//! machine are CRLF. Nothing here can normalise anything, because nothing here
//! writes a byte it was not handed.
//!
//! [`kv`](super::kv) finds those spans by reading one line at a time;
//! [`toml_keys`](super::toml_keys) finds them by asking a TOML parser where
//! each value is. Both end here.

use std::ops::Range;

/// Replace each span of the text with what goes there, and copy the rest.
///
/// Spans may be empty, which inserts without replacing. Two edits reaching the
/// same bytes — a setting set and removed in one pass, which only a
/// hand-written list could ask for — resolve to the first of them rather than
/// writing inside what the other took out.
pub fn patch<S: AsRef<str>>(text: &str, mut cuts: Vec<(Range<usize>, S)>) -> String {
    // Stable, so two inserts at one point keep the order they were asked for.
    cuts.sort_by_key(|(span, _)| span.start);

    let mut out = String::with_capacity(text.len() + 64);
    let mut at = 0;
    for (span, with) in &cuts {
        if span.start < at {
            continue;
        }
        out.push_str(&text[at..span.start]);
        out.push_str(with.as_ref());
        at = span.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The file's own line ending, taken from its first line. 2 of 34 real
/// `.properties` files and 544 of 753 real TOML files are CRLF.
pub fn line_ending(text: &str) -> &'static str {
    match text.split_once('\n') {
        Some((first, _)) if first.ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

/// The whole line holding this span, its terminator and all — or the whole run
/// of lines, when the span covers more than one, as an array written over
/// several lines does.
pub fn line_of(text: &str, inner: Range<usize>) -> Range<usize> {
    let start = text[..inner.start].rfind('\n').map_or(0, |at| at + 1);
    let end = text[inner.end..]
        .find('\n')
        .map_or(text.len(), |at| inner.end + at + 1);
    start..end
}

/// What taking a line out takes out: the line, and the terminator above it
/// when this line has none of its own — or a file that ended without a final
/// newline would gain one.
pub fn removal(text: &str, line: Range<usize>) -> Range<usize> {
    if text[line.clone()].ends_with('\n') {
        return line;
    }
    let mut start = line.start;
    if text[..start].ends_with("\r\n") {
        start -= 2;
    } else if text[..start].ends_with('\n') {
        start -= 1;
    }
    start..line.end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing outside the spans handed over is written, whatever the file
    /// looks like between them — which is what both formats' preservation
    /// rests on.
    #[test]
    fn only_the_spans_handed_over_are_rewritten() {
        let text = "a = 1\r\n# a comment\r\n\tb = 2";
        let out = patch(text, vec![(25..26, "9")]);
        assert_eq!(out, "a = 1\r\n# a comment\r\n\tb = 9");

        // An empty span inserts, and the bytes either side of it stay put.
        let out = patch(text, vec![(7..7, "# added\r\n")]);
        assert_eq!(out, "a = 1\r\n# added\r\n# a comment\r\n\tb = 2");
    }

    /// A line is found whole from any span inside it, including a value
    /// written over several lines, and taking it out never changes whether the
    /// file ends with a newline.
    #[test]
    fn a_line_is_found_whole_and_taking_it_out_keeps_the_files_ending() {
        let text = "a = 1\nb = [\n  1,\n]\nc = 3";
        // The span of `[\n  1,\n]`, which is three lines of the file.
        assert_eq!(line_of(text, 10..18), 6..19);
        // A last line with no terminator of its own takes the one above it.
        assert_eq!(line_of(text, 23..24), 19..24);
        assert_eq!(removal(text, 19..24), 18..24);
        assert_eq!(patch(text, vec![(removal(text, 19..24), "")]), "a = 1\nb = [\n  1,\n]");

        let crlf = "a = 1\r\nb = 2";
        assert_eq!(removal(crlf, 7..12), 5..12);
        assert_eq!(line_ending(crlf), "\r\n");
        assert_eq!(line_ending("a = 1\nb = 2"), "\n");
    }
}
