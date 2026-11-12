//! Start-tag scanner and attribute tokenizer.
//!
//! This module houses the shared state machine that walks the UTF-8 source
//! buffer by byte offset. Task 5.1 implements its first, foundational
//! component: **opaque-region recognition**. An opaque region is any run of
//! bytes the scanner must copy verbatim and never scan for start tags —
//! comments, CDATA sections, doctype/declarations, processing instructions, the
//! bodies of HTML raw-text / escapable-raw-text elements (`script`, `style`,
//! `textarea`, `title`), and dialect template/interpolation regions (Astro
//! frontmatter). Skipping these is what makes the "preserve everything outside
//! start tags byte-for-byte" guarantee structural (Requirements 3.6, 7.4).
//!
//! The recognizer is exposed as [`opaque_region_at`], a small, composable
//! function: given a byte offset and the dialect's [`ScannerProfile`], it
//! reports whether an opaque region begins exactly at that offset and, if so,
//! where it ends. Task 5.2 (`scan_start_tags`) drives the main byte walk and
//! calls this at every candidate position, jumping past any region it reports
//! before it ever considers a `<` as a possible start tag.
//!
//! Unterminated opaque regions at end of input are reported, not silently
//! consumed, so task 5.2 can convert them into a `ParseFailure` carrying the
//! line/column of the opener (Requirement 7.1).

use crate::dialect::{OpaqueRegion, ScannerProfile};
use crate::model::{Attribute, AttributeKind, NestedChild, ScannedTag};

/// The outcome of probing a byte offset for the start of an opaque region.
///
/// Returned by [`opaque_region_at`]. A `None` result from that function means no
/// opaque region begins at the probed offset; a `Some(OpaqueMatch)` describes a
/// region that does, and whether its closer was found before end of input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpaqueMatch {
    /// Which kind of opaque region was recognized at the probed offset.
    pub kind: OpaqueRegion,
    /// Byte offset of the first byte of the region (the probed offset).
    pub start: usize,
    /// Byte offset one past the region's closer when the region is terminated,
    /// or one past the end of input (`src.len()`) when it is not. Either way
    /// this is always a valid, in-bounds-or-equal-to-`len` offset the caller can
    /// resume scanning from without re-entering the region.
    pub end: usize,
    /// Whether the region's closer was found before end of input. When `false`
    /// the region ran off the end of the buffer unterminated; task 5.2 converts
    /// that into a `ParseFailure` located at [`start`](Self::start) for the
    /// comment, CDATA, and raw-text-element kinds enumerated by Requirement 7.1,
    /// and treats the always-terminable kinds (declaration, processing
    /// instruction, frontmatter) leniently.
    pub terminated: bool,
}

/// Probe whether an opaque region begins exactly at byte offset `at` in `src`
/// under the dialect rules in `profile`.
///
/// Returns `Some(OpaqueMatch)` when one of the profile's
/// [`opaque_regions`](ScannerProfile::opaque_regions) opens at `at`, or `None`
/// when the bytes at `at` do not begin any opaque region this dialect
/// recognizes (in which case the caller inspects the position itself — e.g. as a
/// possible start tag).
///
/// The returned [`OpaqueMatch::end`] is where the caller should resume scanning:
/// one past the region's closer when terminated, or `src.len()` when the region
/// is unterminated at end of input. The function never panics and always
/// produces an `end` in `at..=src.len()`, so it composes cleanly into the main
/// byte walk (task 5.2).
///
/// `at` must be a byte offset that lies on a UTF-8 character boundary within
/// `src`; task 5.2 only ever probes boundaries it is already standing on.
#[must_use]
pub fn opaque_region_at(src: &str, at: usize, profile: &ScannerProfile) -> Option<OpaqueMatch> {
    let bytes = src.as_bytes();
    if at >= bytes.len() {
        return None;
    }

    for &kind in profile.opaque_regions {
        if let Some(opener_len) = opener_len_at(kind, src, at) {
            let (end, terminated) = find_region_end(kind, src, at, opener_len);
            return Some(OpaqueMatch {
                kind,
                start: at,
                end,
                terminated,
            });
        }
    }
    None
}

/// If the opaque region `kind` opens at byte offset `at`, return the length in
/// bytes of its opener; otherwise `None`.
///
/// Recognizing the opener is deliberately separate from finding the closer so
/// the two concerns stay readable and independently testable.
fn opener_len_at(kind: OpaqueRegion, src: &str, at: usize) -> Option<usize> {
    let rest = &src.as_bytes()[at..];
    match kind {
        OpaqueRegion::Comment => starts_with(rest, b"<!--").then_some(4),
        OpaqueRegion::CData => starts_with(rest, b"<![CDATA[").then_some(9),
        // A declaration is `<!` not already claimed as a comment or CDATA. The
        // comment/CDATA kinds are listed before `Declaration` in every markup
        // profile, so when probing `Declaration` those more specific openers
        // have already been given their chance; still, guard here so the order
        // of `opaque_regions` cannot silently mis-classify `<!--`/`<![CDATA[`.
        OpaqueRegion::Declaration => {
            (starts_with(rest, b"<!")
                && !starts_with(rest, b"<!--")
                && !starts_with(rest, b"<![CDATA["))
            .then_some(2)
        }
        OpaqueRegion::ProcessingInstruction => starts_with(rest, b"<?").then_some(2),
        // A raw-text element opaque region opens at the `<` of a `<script>`,
        // `<style>`, `<textarea>`, or `<title>` start tag. The opener length is
        // the full start tag (up to and including its `>`), since the opaque
        // body begins only after the start tag closes.
        OpaqueRegion::RawTextElement => raw_text_opener_len(src, at),
        // Astro frontmatter is a `---` fence, recognized only at the very start
        // of the file (offset 0), optionally after a leading newline is not
        // allowed — frontmatter must lead the document.
        OpaqueRegion::Frontmatter => {
            (at == 0 && starts_with(rest, b"---")).then_some(3)
        }
    }
}

/// Find where an opaque region of `kind` ends, given that its opener of
/// `opener_len` bytes starts at `at`.
///
/// Returns `(end, terminated)` where `end` is one past the closer (or
/// `src.len()` if unterminated) and `terminated` reports whether the closer was
/// found before end of input.
fn find_region_end(
    kind: OpaqueRegion,
    src: &str,
    at: usize,
    opener_len: usize,
) -> (usize, bool) {
    let bytes = src.as_bytes();
    let body_start = at + opener_len;
    match kind {
        OpaqueRegion::Comment => close_after(bytes, body_start, b"-->"),
        OpaqueRegion::CData => close_after(bytes, body_start, b"]]>"),
        OpaqueRegion::Declaration => close_after(bytes, body_start, b">"),
        OpaqueRegion::ProcessingInstruction => close_after(bytes, body_start, b"?>"),
        OpaqueRegion::RawTextElement => {
            // `at..body_start` is the raw-text element's start tag; the element
            // name is the ASCII run just after the `<`. The opaque body runs to
            // the matching `</name` end tag (case-insensitive), whose `>` closes
            // the region. The name is guaranteed present: `opener_len_at` only
            // returned `Some` for `RawTextElement` because it found the name.
            match raw_text_element_name(src, at) {
                Some(name) => find_raw_text_close(src, body_start, name),
                // Unreachable in practice; stay panic-free by treating a missing
                // name as an unterminated region to end of input.
                None => (bytes.len(), false),
            }
        }
        OpaqueRegion::Frontmatter => {
            // The closing fence is a `---` on its own line after the opening
            // fence. Search for a `\n---` sequence; the region ends after that
            // fence line's trailing newline when present, else at the fence.
            find_frontmatter_close(src, body_start)
        }
    }
}

/// Whether `haystack` begins with `needle`.
fn starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && &haystack[..needle.len()] == needle
}

/// Find the first occurrence of `closer` at or after `from` in `bytes`.
///
/// Returns `(one-past-closer, true)` when found, or `(bytes.len(), false)` when
/// the closer never appears before end of input (an unterminated region).
fn close_after(bytes: &[u8], from: usize, closer: &[u8]) -> (usize, bool) {
    match find_subslice(bytes, from, closer) {
        Some(idx) => (idx + closer.len(), true),
        None => (bytes.len(), false),
    }
}

/// Index of the first occurrence of `needle` in `bytes` at or after `from`, or
/// `None`. A naive forward scan; `needle` is always a short fixed literal here,
/// so this stays linear in the region length with no backtracking.
fn find_subslice(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    let last_start = bytes.len().checked_sub(needle.len())?;
    (from..=last_start).find(|&i| &bytes[i..i + needle.len()] == needle)
}

/// The set of HTML raw-text / escapable-raw-text element names whose bodies are
/// opaque (Requirement 3.6). Matched case-insensitively.
const RAW_TEXT_ELEMENTS: [&str; 4] = ["script", "style", "textarea", "title"];

/// If a raw-text element start tag opens at `at` (the `<`), return the byte
/// length of that whole start tag (through its closing `>`); otherwise `None`.
///
/// The tag qualifies only when the name immediately after `<` is one of
/// [`RAW_TEXT_ELEMENTS`] (case-insensitive) AND is followed by a tag-name
/// boundary — whitespace, `>`, or `/` — so that `<scripting>` is not mistaken
/// for `<script>`.
fn raw_text_opener_len(src: &str, at: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(at) != Some(&b'<') {
        return None;
    }
    let name = raw_text_element_name(src, at)?;
    if !RAW_TEXT_ELEMENTS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(name))
    {
        return None;
    }
    // The start tag ends at the first `>` at or after the name. Raw-text element
    // start tags do not contain `>`-bearing braced expressions, so a plain
    // search for `>` is correct and keeps this component simple.
    let name_end = at + 1 + name.len();
    match find_subslice(bytes, name_end, b">") {
        Some(gt) => Some(gt + 1 - at),
        // An unterminated start tag (`<script` with no `>`): there is no body to
        // skip. Report an opener spanning the rest of the input so the caller
        // sees an unterminated region located at the `<`.
        None => Some(bytes.len() - at),
    }
}

/// The element-name slice immediately following the `<` at `at`, or `None` when
/// there is no ASCII-alphabetic name there.
///
/// The name is the maximal run of ASCII alphanumerics after the `<`; the caller
/// decides whether that name is a raw-text element.
fn raw_text_element_name(src: &str, at: usize) -> Option<&str> {
    let bytes = src.as_bytes();
    let name_start = at + 1;
    if bytes.get(name_start).is_none_or(|b| !b.is_ascii_alphabetic()) {
        return None;
    }
    let mut end = name_start;
    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    Some(&src[name_start..end])
}

/// Find the end of a raw-text element body that began after its start tag at
/// `body_start`, closing at the matching `</name>` end tag (case-insensitive).
///
/// Returns `(one-past-the-end-tag-`>`, true)` when the end tag is found, or
/// `(src.len(), false)` when the body runs unterminated to end of input.
fn find_raw_text_close(src: &str, body_start: usize, name: &str) -> (usize, bool) {
    let bytes = src.as_bytes();
    let mut i = body_start;
    while i < bytes.len() {
        match find_subslice(bytes, i, b"</") {
            Some(lt) => {
                let after = lt + 2;
                if region_name_matches(src, after, name) {
                    // Advance to the `>` that closes this end tag.
                    let name_end = after + name.len();
                    return close_after(bytes, name_end, b">");
                }
                // A `</` that is not this element's end tag (e.g. `</span>`
                // nested in a `<script>` string): skip past it and keep looking.
                i = after;
            }
            None => return (bytes.len(), false),
        }
    }
    (bytes.len(), false)
}

/// Whether the bytes at `after` begin with `name` (case-insensitive) followed by
/// an end-tag boundary (whitespace or `>`), so `</script>` matches but
/// `</scripting>` does not.
fn region_name_matches(src: &str, after: usize, name: &str) -> bool {
    let bytes = src.as_bytes();
    let name_end = after + name.len();
    if name_end > bytes.len() {
        return false;
    }
    if !src[after..name_end].eq_ignore_ascii_case(name) {
        return false;
    }
    match bytes.get(name_end) {
        None => true,
        Some(b) => b.is_ascii_whitespace() || *b == b'>',
    }
}

/// Find the closing `---` fence of Astro frontmatter that opened at offset 0.
///
/// Searches for a `---` fence on its own line after `body_start`. Returns
/// `(one-past-the-fence-line, true)` on success (consuming the fence's trailing
/// newline when present so scanning resumes on the next line), or
/// `(src.len(), false)` when no closing fence appears — treated leniently by
/// task 5.2 rather than as a hard parse failure.
fn find_frontmatter_close(src: &str, body_start: usize) -> (usize, bool) {
    let bytes = src.as_bytes();
    match find_subslice(bytes, body_start, b"\n---") {
        Some(nl) => {
            let fence_end = nl + 4; // past "\n---"
            // Consume to the end of the fence line (its trailing newline).
            match find_subslice(bytes, fence_end, b"\n") {
                Some(line_nl) => (line_nl + 1, true),
                None => (bytes.len(), true),
            }
        }
        None => (bytes.len(), false),
    }
}

// ---------------------------------------------------------------------------
// Start-tag location (`scan_start_tags`, task 5.2)
// ---------------------------------------------------------------------------

/// A hard parse failure reported by [`scan_start_tags`].
///
/// Hard failures are limited to the enumerated end-of-input truncation triggers
/// of Requirement 7.1: an unterminated start tag, an unterminated quoted
/// attribute value, an unterminated braced expression region, and an
/// unterminated opaque region (comment, CDATA, or raw-text element body) at end
/// of input. Every other ambiguous construct is handled leniently rather than
/// failing hard.
///
/// [`line`](Self::line) and [`column`](Self::column) are **1-based** and always
/// in-bounds: they are derived from a byte offset within the source by counting
/// newlines, and an offset at end of input maps to the line/column just past the
/// last byte on its line (never beyond a valid location).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseFailure {
    /// 1-based line number of the failure location.
    pub line: usize,
    /// 1-based column number of the failure location, counted in characters
    /// (code points) from the start of the line.
    pub column: usize,
    /// Human-readable description of what was left unterminated.
    pub message: String,
}

impl ParseFailure {
    /// Build a [`ParseFailure`] at the 1-based line/column corresponding to byte
    /// `offset` in `src`, with the given message.
    ///
    /// `offset` is clamped to `src.len()` so the computed location is always
    /// in-bounds even if a caller passes an offset past the end. The column is
    /// counted in code points from the last newline before `offset`.
    fn at(src: &str, offset: usize, message: impl Into<String>) -> Self {
        let offset = offset.min(src.len());
        // Ensure we land on a char boundary at or before `offset` so slicing is
        // always valid; byte offsets handed in here are already boundaries, but
        // clamp defensively.
        let mut bound = offset;
        while bound > 0 && !src.is_char_boundary(bound) {
            bound -= 1;
        }
        let prefix = &src[..bound];
        let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |nl| nl + 1);
        let column = src[line_start..bound].chars().count() + 1;
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

/// One active child-position `{ … }` JSX expression container the
/// [`scan_start_tags`] walk is currently inside.
///
/// The walk keeps a stack of these (one per nesting level) so each container
/// tracks its own interior bracket nesting independently: the closing `}` of an
/// inner container (e.g. `{…<Cell/>}` nested inside `{rows.map(r => <Row>{…}
/// </Row>)}`) is matched against the inner frame's own interior count, never
/// conflated with the outer container's interior brackets.
#[derive(Clone, Copy, Debug)]
struct ContainerFrame {
    /// Byte offset of this container's opening `{`, used to locate an
    /// unterminated-at-EOF hard failure (Requirement 7.1) at the opener.
    open_at: usize,
    /// Depth of interior `{`/`(`/`[` nesting within this container. The
    /// container's own closing `}` is the one reached while this is zero.
    interior_depth: usize,
}

/// The outcome of advancing one byte while inside a child-position `{ … }` JSX
/// expression container (see [`step_in_container`]).
enum InteriorStep {
    /// The interior lexer consumed one or more bytes. The caller sets
    /// `i = next.max(i + 1)`, `last_significant = last`, `regex_allowed = regex`,
    /// and continues the main loop.
    Advance {
        next: usize,
        last: LastToken,
        regex: bool,
    },
    /// The byte is not one the interior lexer handles (a string, template,
    /// identifier, `<`, or other byte). The caller falls through to the shared
    /// string/template/identifier/`<`-gate paths of the main loop.
    Fallthrough,
}

/// Advance the walk by one step while it is inside a child-position `{ … }` JSX
/// expression container (the stack is non-empty).
///
/// Mirrors [`delimit_braced`]'s token discipline so an interior `}` buried in a
/// string / template / regex / comment, or inside a nested `{}`/`()`/`[]`, never
/// prematurely closes the container: interior brackets adjust the current
/// frame's `interior_depth`; the container's own `}` (reached while that depth is
/// zero) pops the frame; regex literals and line/block comments are skipped as
/// whole tokens. Strings, templates, identifiers, and `<` are left for the main
/// loop's shared paths (returned as [`InteriorStep::Fallthrough`]).
///
/// Returns a [`ParseFailure`] only for an unterminated block comment at end of
/// input (Requirement 7.1), located at the current container's opening `{`.
fn step_in_container(
    src: &str,
    i: usize,
    regex_allowed: bool,
    container_stack: &mut Vec<ContainerFrame>,
) -> Result<InteriorStep, ParseFailure> {
    let bytes = src.as_bytes();
    let frame = container_stack
        .last_mut()
        .expect("step_in_container is only called with a non-empty container stack");
    let b = bytes[i];
    match b {
        // Interior opener: nest one level deeper within the current frame. (A
        // child-position `{` was already handled by the entry branch; reaching
        // here with `{` means it is at `OperatorOrOpen`/`Ident` — an interior
        // object literal / block / arrow body — so it is interior nesting.)
        b'{' | b'(' | b'[' => {
            frame.interior_depth += 1;
            Ok(InteriorStep::Advance {
                next: i + 1,
                last: LastToken::OperatorOrOpen,
                regex: true,
            })
        }
        b'}' if frame.interior_depth == 0 => {
            // The matching close of the current child container. Pop the frame
            // and resume at child position so the next sibling `<li>`/`{expr}` is
            // handled correctly.
            container_stack.pop();
            Ok(InteriorStep::Advance {
                next: i + 1,
                last: LastToken::TagClose,
                regex: false,
            })
        }
        b'}' | b')' | b']' => {
            // Close of an interior nested region: pop one interior level.
            frame.interior_depth = frame.interior_depth.saturating_sub(1);
            // A closer yields a value, so a following `/` is division and a
            // following `<` is a generic (`last_significant == Ident`).
            Ok(InteriorStep::Advance {
                next: i + 1,
                last: LastToken::Ident,
                regex: false,
            })
        }
        // Line comment: skip it whole so a `}` inside `// ...` cannot close the
        // container. A newline permits a regex after.
        b'/' if bytes.get(i + 1) == Some(&b'/') => Ok(InteriorStep::Advance {
            next: skip_line_comment(bytes, i + 2, bytes.len()),
            last: LastToken::OperatorOrOpen,
            regex: true,
        }),
        // Block comment: skip it whole so a `}` inside `/* ... */` cannot close
        // the container. Unterminated at EOF is a hard parse failure (Req 7.1).
        b'/' if bytes.get(i + 1) == Some(&b'*') => match skip_block_comment(bytes, i + 2, bytes.len())
        {
            Some(next) => Ok(InteriorStep::Advance {
                next,
                last: LastToken::OperatorOrOpen,
                regex: false,
            }),
            None => Err(ParseFailure::at(
                src,
                frame.open_at,
                "unterminated braced expression at end of input",
            )),
        },
        // Regex literal (only where a `/` may start one): skip it whole so an
        // interior `}` in `/a}b/` cannot close the container. On an unterminated
        // regex, treat `/` as ordinary division (stay conservative).
        b'/' if regex_allowed => match scan_regex_literal(src, i + 1, bytes.len()) {
            Some(next) => Ok(InteriorStep::Advance {
                next,
                last: LastToken::Ident,
                regex: false,
            }),
            None => Ok(InteriorStep::Advance {
                next: i + 1,
                last: LastToken::OperatorOrOpen,
                regex: true,
            }),
        },
        // Division operator; an operator means a regex may follow.
        b'/' => Ok(InteriorStep::Advance {
            next: i + 1,
            last: LastToken::OperatorOrOpen,
            regex: true,
        }),
        // Everything else is handled by the main loop's shared paths.
        _ => Ok(InteriorStep::Fallthrough),
    }
}

/// Scan `src` for start tags under the dialect rules in `profile`, using the
/// tab = `indent_width` column model for the `open_col`/`base_indent_col`
/// fields.
///
/// Walks the UTF-8 buffer by byte offset. At each position it first asks
/// [`opaque_region_at`] whether an opaque region begins there; if so it jumps
/// past the region (converting an unterminated comment / CDATA / raw-text body
/// into a [`ParseFailure`] per Requirement 7.1) without ever scanning its
/// contents for tags. Otherwise, when a `<` is followed by a dialect-admitted
/// name-start character, it delimits the whole start tag — consuming to the
/// matching `>` while skipping `>` inside quoted values and balanced
/// `{...}`/`(...)` regions — and records the tag's span, name, self-closing
/// flag, and column geometry.
///
/// Attribute tokenization is task 5.3; for now each tag carries an empty
/// attribute list, populated later by [`tokenize_attributes`].
///
/// Returns the ordered start tags, or a [`ParseFailure`] for one of the
/// enumerated end-of-input truncation triggers.
pub fn scan_start_tags<'a>(
    src: &'a str,
    profile: &ScannerProfile,
    indent_width: usize,
) -> Result<Vec<ScannedTag<'a>>, ParseFailure> {
    let bytes = src.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0usize;
    // Drift-proof running classification of the most recent significant token at
    // top-level (child) position (Change 2, Requirement 2.5). This is a pure
    // "what did I last see", recomputed continuously from the bytes consumed —
    // never a nesting counter — so it cannot accumulate error across fragments,
    // self-closing tags, or skipped generics. It gates the JSX `<` start-tag
    // decision (via `is_start_tag_position`) and determines where a child
    // container is entered.
    let mut last_significant = LastToken::None;

    // Depth of child-position `{ ... }` JSX expression containers the walk is
    // currently inside (0 = at document child level). Incremented when a
    // child-container `{` is entered (Change A: walk-into-container, replacing
    // the former opaque skip), decremented when its matching `}` is reached.
    //
    // This counter is a BOUNDARY device, not a GATE device: it NEVER
    // participates in the `<`-as-start-tag decision (that stays a pure function
    // of `last_significant`). It only records where a container ends so the
    // `last_significant` resumed AFTER the container is `TagClose` (child
    // position). A miscount can at worst pick `TagClose` vs `OperatorOrOpen` at
    // a boundary — both expression-introducer classes that accept a genuine `<`
    // and reject an `Ident<` generic — so it can never turn a generic into a
    // tag or vice versa (see design "Why the depth counter does not reintroduce
    // drift"). Only literal child-position `{`/matching `}` touch it, so it has
    // no cross-tag accumulation path.
    //
    // Each active child container carries its OWN interior bracket-nesting count
    // and its opening-`{` offset, kept on `container_stack`. `scan_start_tags`
    // tracks `child_container_depth == container_stack.len()`. A nested child
    // container (one opened at child position WHILE already inside a container,
    // e.g. the `{…<Cell/>}` inside `{rows.map(r => <Row>{…}</Row>)}`) pushes a
    // fresh frame with its own interior count of 0, so the inner container's
    // closing `}` is never confused with the outer container's interior
    // brackets.
    let mut container_stack: Vec<ContainerFrame> = Vec::new();
    // Whether a `/` at the current position may begin a regex literal (true
    // right after an operator/opener/at the start) versus a division operator
    // (false right after a value/closer). Only consulted while inside a child
    // container, where the walk must distinguish `{re.test(/a}b/)}` (regex) from
    // division so an interior `}` in a regex never closes the container early.
    let mut regex_allowed = true;

    while i < bytes.len() {
        // Opaque regions take precedence: never scan their contents for tags.
        if let Some(region) = opaque_region_at(src, i, profile) {
            if !region.terminated && is_hard_failure_region(region.kind) {
                return Err(ParseFailure::at(
                    src,
                    region.start,
                    unterminated_region_message(region.kind),
                ));
            }
            // Terminated region, or an always-terminable kind treated leniently:
            // resume past it. `end` is guaranteed in `start..=len`, and strictly
            // greater than `i` for any real opener, so the walk makes progress.
            i = region.end.max(i + 1);
            // Keep the gate's context correct after a skip: a comment is
            // whitespace-like and leaves the preceding token in force, while
            // every other opaque region (raw-text element body, CDATA,
            // declaration, processing instruction, frontmatter) is followed by
            // markup content — child position — so a following `<`/`{` is a tag
            // / child container (`TagClose`).
            if region.kind != OpaqueRegion::Comment {
                last_significant = LastToken::TagClose;
            }
            // An opaque region boundary is followed by markup/expression
            // context, so a `/` after it could begin a regex.
            regex_allowed = true;
            continue;
        }

        // Change A — Walk INTO a child-position `{ ... }` JSX expression
        // container instead of skipping it as opaque (Requirements 2.1–2.8).
        //
        // The former behavior delimited the whole container with `delimit_braced`
        // and jumped past it, hiding every genuine JSX element inside a `.map()`
        // callback, ternary, `&&`/`||` conditional, or nested container. Now the
        // scanner ENTERS the container and keeps walking: its interior is
        // ordinary JS/TS expression text subject to the SAME Change 2
        // expression-position gate (`is_start_tag_position` + `last_significant`),
        // so a genuine JSX `<` inside is recognized and a TypeScript generic /
        // `<` operator inside is rejected — exactly as outside a container.
        //
        // The entry fires ONLY at genuine JSX child position (`None | TagClose`),
        // never for a statement-block or function/arrow body `{` (which follow
        // `Ident`/`OperatorOrOpen`): those are already walked by the ordinary
        // path and their `return <div>` / `yield <x/>` JSX is recognized. An
        // INTERIOR `{` (at `OperatorOrOpen`/`Ident`, e.g. an object literal or
        // arrow body) does not match this guard, so it is counted as interior
        // nesting below rather than as a new container boundary.
        if profile.scan_child_expression_containers
            && bytes[i] == b'{'
            && matches!(last_significant, LastToken::None | LastToken::TagClose)
        {
            container_stack.push(ContainerFrame {
                open_at: i,
                interior_depth: 0,
            });
            i += 1; // consume the `{`
            // A `{` introduces an expression: a following `<` is a genuine JSX
            // element (expression position) and a following `/` may be a regex.
            last_significant = LastToken::OperatorOrOpen;
            regex_allowed = true;
            continue;
        }

        // While inside a child container, the interior is JS/TS expression text.
        // Mirror `delimit_braced`'s token discipline so an interior `}` buried in
        // a string / template / regex / comment, or inside a nested
        // `{}`/`()`/`[]`, never prematurely closes the container. The ordinary
        // byte path below already consumes `'`/`"` strings and `` ` `` templates
        // whole; here we additionally consume regex literals and line/block
        // comments and track interior bracket nesting. The `<` gate still applies
        // (handled by the shared `<` branch below), so genuine JSX is recognized
        // and generics are rejected identically to outside a container.
        if !container_stack.is_empty() {
            match step_in_container(src, i, regex_allowed, &mut container_stack)? {
                InteriorStep::Advance {
                    next,
                    last,
                    regex,
                } => {
                    i = next.max(i + 1);
                    last_significant = last;
                    regex_allowed = regex;
                    continue;
                }
                // Not consumed by the interior lexer (strings, templates,
                // identifiers, `<`, other bytes): fall through to the shared
                // paths below, which consume strings/templates whole and apply
                // the `<` gate, updating `regex_allowed` from the classification.
                InteriorStep::Fallthrough => {}
            }
        }

        if bytes[i] == b'<' {
            if is_start_tag_position(src, i, last_significant, profile) {
                let tag = delimit_start_tag(src, i, profile, indent_width)?;
                let next = tag.span.end;
                tags.push(tag);
                // `delimit_start_tag` always consumes at least the `<`, through
                // the tag-closing `>`. Child position follows a recognized
                // start/opening or self-closing tag, so the tag-closing `>`
                // yields `TagClose` — which takes precedence over the generic
                // `OperatorOrOpen` classification a bare `>` operator would get,
                // letting a following child element (e.g. the `<li>` after
                // `<ul>`) be accepted.
                i = next.max(i + 1);
                last_significant = LastToken::TagClose;
                // A recognized tag yields a value-like child position; a
                // following `/` inside a container is division.
                regex_allowed = false;
                continue;
            }
            // Rejected: this `<` is a TypeScript generic-argument opener or a
            // less-than operator in value position (`Ident`), never a JSX
            // element. Advance past the `<` by exactly one code point, leaving
            // the `<`, the generic head, and every `>` as verbatim non-tag
            // bytes. A `<` is an expression introducer, so a following token is
            // in operator position; nested generics (`useState<Array<Task>>`)
            // are preserved because each inner `<` is likewise in `Ident`
            // position and every `>` is ordinary text.
            i += 1;
            last_significant = LastToken::OperatorOrOpen;
            // A rejected `<` is a generic/less-than operator. Crucially, when it
            // is the `<` of a JSX CLOSING tag `</name>`, the following `/` must
            // NOT be read as a regex-literal start (which would run off scanning
            // for a closing `/` and corrupt the container walk). Treat the
            // position after a `<` as division context so `</Row>` is consumed
            // as ordinary bytes.
            regex_allowed = false;
            continue;
        }

        // Ordinary (non-`<`, non-skipped) byte: consume it and recompute the
        // running classification. Strings and template literals are consumed
        // whole so a `<` inside them is never mistaken for a tag, and because
        // they produce a value (`Ident`). Everything else maps to `Ident`
        // (value producer) or `OperatorOrOpen` (expression introducer) via
        // `classify_byte`; whitespace and other non-significant bytes leave the
        // classification unchanged.
        let b = bytes[i];
        match b {
            b'\'' | b'"' => match scan_js_string(src, i + 1, b, bytes.len()) {
                // A terminated string / char literal produces a value.
                Some(next) => {
                    i = next.max(i + 1);
                    last_significant = LastToken::Ident;
                    regex_allowed = false;
                }
                // Unterminated at end of input (e.g. an apostrophe in JSX text
                // such as `Don't`): treat the quote as an ordinary byte and
                // advance one code point, leaving the classification in force.
                None => i += 1,
            },
            b'`' => match scan_template_literal_whole(src, i + 1, bytes.len()) {
                Some(next) => {
                    i = next.max(i + 1);
                    last_significant = LastToken::Ident;
                    regex_allowed = false;
                }
                None => i += 1,
            },
            _ if is_ident_start_byte(b) => {
                // Consume the whole identifier / keyword run so a keyword that
                // introduces an expression (`return`, `yield`, `await`, …) is
                // classified as an introducer rather than as a value producer:
                // a per-byte view would see only the final letter and wrongly
                // mark `return <div>` as `Ident`, rejecting the genuine JSX.
                let word_end = scan_ident_run(bytes, i);
                let word = &src[i..word_end];
                if is_expression_keyword(word) {
                    last_significant = LastToken::OperatorOrOpen;
                    regex_allowed = true;
                } else {
                    last_significant = LastToken::Ident;
                    regex_allowed = false;
                }
                i = word_end.max(i + 1);
            }
            _ => {
                if let Some(classified) = classify_byte(b) {
                    last_significant = classified;
                    // Keep the regex-vs-division context in sync with the
                    // classification for the in-container lexer: a value
                    // producer (`Ident`) forbids a following regex; an operator
                    // / opener permits one. Whitespace (`None`) is transparent.
                    regex_allowed = classified == LastToken::OperatorOrOpen;
                }
                // Advance by one whole code point so `i` stays on a char
                // boundary.
                i += utf8_len(b);
            }
        }
    }

    // A child container whose matching `}` was never reached before end of
    // input is an unterminated braced expression (Requirement 7.1), reported at
    // the outermost unterminated container's opening `{` — the same hard failure
    // the former opaque-skip raised via `delimit_braced`.
    if let Some(outer) = container_stack.first() {
        return Err(ParseFailure::at(
            src,
            outer.open_at,
            "unterminated braced expression at end of input",
        ));
    }

    Ok(tags)
}

/// Tokenize the attribute region of a start tag into ordered
/// [`Attribute`](crate::model::Attribute) tokens (task 5.3).
///
/// `attrs_range` is the byte range between the end of `<name` and the tag
/// terminator (`>` or `/>`); [`delimit_start_tag`] passes `name_end..span.end`.
/// This function first trims the trailing terminator and the inter-attribute
/// whitespace, then walks the remaining bytes splitting them into attributes.
///
/// Three forms are produced, gated by the dialect's
/// [`AttributeForms`](crate::dialect::AttributeForms):
///
/// - **Plain** — a bare name with no `=`, e.g. `disabled`. `name_key` is the
///   lowercased name.
/// - **Name-value** — `name = value` where the value is quoted (`"..."`,
///   `'...'`), unquoted (HTML only), or a braced expression (`class={x}`,
///   `v-if="x"`, `on:click={...}`). The whole `name…value` run is captured
///   verbatim and `name_key` is the lowercased name.
/// - **Dynamic / spread** — a brace-led spread such as `{...props}` (JSX/Astro/
///   Svelte) captured verbatim with `name_key = None`. Name-led dynamic
///   directives such as `v-bind="obj"` keep their name and so tokenize as a
///   name-value attribute with a `name_key`.
///
/// Each attribute's [`raw`](crate::model::Attribute::raw) slice is the exact
/// source bytes, never reconstructed from parts, and the interior of braced /
/// spread values is never parsed as an expression (Requirements 3.6, 4.6, 5.3,
/// 7.4). The width is the shared [`display_width`](crate::model::display_width)
/// of that raw slice.
///
/// The JSX/TSX scanner-confidence model and the `Complex_Expression_Attribute`
/// flagging are task 5.4; this function focuses on tokenizing the common forms
/// and leaves room for 5.4 to layer confidence tracking on top.
#[must_use]
pub fn tokenize_attributes<'a>(
    src: &'a str,
    attrs_range: std::ops::Range<usize>,
    profile: &ScannerProfile,
    indent_width: usize,
) -> Vec<crate::model::Attribute<'a>> {
    use crate::model::display_width;

    let bytes = src.as_bytes();
    // Clamp the range defensively so slicing is always valid.
    let region_start = attrs_range.start.min(src.len());
    let region_end = attrs_range.end.min(src.len());
    if region_start >= region_end {
        return Vec::new();
    }

    // Trim the trailing terminator (`>` or `/>`) from the region so it is never
    // mistaken for attribute content.
    let end = trim_terminator(bytes, region_start, region_end);

    let mut attrs = Vec::new();
    let mut i = region_start;
    while i < end {
        // Skip inter-attribute whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // A brace-led token is a pure spread/dynamic attribute: `{...props}`.
        if bytes[i] == b'{' && profile.attribute_forms.spread {
            // Delimit with the confidence-aware JS lexer. When it cannot resolve
            // the braced region with certainty the attribute is a
            // `Complex_Expression_Attribute`; fall back to best-effort balanced
            // scanning for its span and let the kind carry the uncertainty.
            let (attr_end, confident) = delimit_braced(src, i, end);
            let raw = &src[i..attr_end];
            attrs.push(Attribute {
                kind: if confident {
                    AttributeKind::DynamicSpread
                } else {
                    AttributeKind::ComplexExpression
                },
                name_key: None,
                raw,
                width: display_width(raw, indent_width),
            });
            i = attr_end.max(i + 1);
            continue;
        }

        // Otherwise an attribute begins with a name. Scan the name, then look for
        // an `=` (allowing whitespace around it) that introduces a value.
        let name_start = i;
        let name_end = scan_attr_name(src, name_start, end);
        if name_end == name_start {
            // Not a name character and not a brace spread — skip one code point
            // so the walk always makes progress and stays panic-free.
            i += utf8_len(bytes[i]);
            continue;
        }
        let name = &src[name_start..name_end];

        // Look past optional whitespace for an `=`.
        let after_name = skip_ws(bytes, name_end, end);
        if after_name < end && bytes[after_name] == b'=' {
            // Name-value attribute: capture through the end of the value.
            let value_start = skip_ws(bytes, after_name + 1, end);
            let (value_end, value_kind) =
                scan_attr_value(src, value_start, end, profile);
            let raw = &src[name_start..value_end];
            attrs.push(Attribute {
                kind: value_kind,
                name_key: Some(name.to_ascii_lowercase()),
                raw,
                width: display_width(raw, indent_width),
            });
            i = value_end.max(i + 1);
        } else {
            // Plain boolean attribute: just the name.
            attrs.push(Attribute {
                kind: AttributeKind::Plain,
                name_key: Some(name.to_ascii_lowercase()),
                raw: name,
                width: display_width(name, indent_width),
            });
            i = name_end.max(i + 1);
        }
    }

    attrs
}

/// Whether every attribute of a tokenized list was delimited with full
/// confidence — i.e. none is a [`AttributeKind::ComplexExpression`].
///
/// Confidence is a property of the entire start tag: a single uncertain braced
/// attribute poisons the whole tag (Requirement 4.9). [`delimit_start_tag`]
/// uses this to set [`ScannedTag::confidently_rewritable`]. There is no
/// file-level aggregate — confidence is tracked and acted on per tag only.
fn attributes_confident(attrs: &[Attribute<'_>]) -> bool {
    attrs
        .iter()
        .all(|a| a.kind != AttributeKind::ComplexExpression)
}

/// Trim the trailing tag terminator (`/>` or `>`) and any whitespace before it
/// from the attribute region `start..end`, returning the offset one past the
/// last attribute byte.
///
/// The region handed in by [`delimit_start_tag`] always ends with the
/// terminator, so removing it here keeps the terminator out of the last
/// attribute's raw slice.
fn trim_terminator(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut e = end;
    // Drop a trailing `>`.
    if e > start && bytes[e - 1] == b'>' {
        e -= 1;
        // Drop the `/` of a `/>` self-closing terminator.
        if e > start && bytes[e - 1] == b'/' {
            e -= 1;
        }
    }
    e
}

/// Scan an attribute name beginning at `from`, returning the offset one past the
/// last name byte (bounded by `end`).
///
/// Attribute names admit the characters used across the dialects: ASCII
/// alphanumerics plus the punctuation that appears in framework directives and
/// namespaced / data attributes — `-`, `_`, `:`, `.`, `$`, `@`, `#`. The leading
/// `=`, whitespace, quotes, braces, and `/`/`>` all terminate the name.
fn scan_attr_name(src: &str, from: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < end {
        let b = bytes[i];
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b':' | b'.' | b'$' | b'@' | b'#')
        {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Scan an attribute value beginning at `from` (the first non-whitespace byte
/// after `=`), returning `(offset-one-past-the-value, kind)` bounded by `end`.
///
/// The value may be a quoted string (`"..."` / `'...'`), a braced expression
/// (`{...}`, when the dialect allows braced values), or — for HTML — an unquoted
/// run up to the next whitespace or terminator. Interiors are never parsed as
/// expressions: the captured slice is preserved verbatim.
///
/// Quoted and unquoted values always delimit confidently and report
/// [`AttributeKind::NameValue`]. A braced value is delimited by the
/// confidence-aware JS lexer ([`delimit_braced`]); when that lexer cannot be
/// certain where the braced region ends, the value is reported as
/// [`AttributeKind::ComplexExpression`] (its span is still a best-effort
/// balanced-brace slice) so [`delimit_start_tag`] can mark the whole tag
/// not-confidently-rewritable (Requirement 4.9).
fn scan_attr_value(
    src: &str,
    from: usize,
    end: usize,
    profile: &ScannerProfile,
) -> (usize, AttributeKind) {
    let bytes = src.as_bytes();
    if from >= end {
        return (from, AttributeKind::NameValue);
    }
    match bytes[from] {
        q @ (b'"' | b'\'') => (
            scan_quoted(src, from + 1, q).unwrap_or(end).min(end),
            AttributeKind::NameValue,
        ),
        b'{' if profile.attribute_forms.braced_value => {
            let (value_end, confident) = delimit_braced(src, from, end);
            let kind = if confident {
                AttributeKind::NameValue
            } else {
                AttributeKind::ComplexExpression
            };
            (value_end, kind)
        }
        _ => {
            // Unquoted value: run up to the next whitespace, `/`, or `>`.
            let mut i = from;
            while i < end {
                let b = bytes[i];
                if b.is_ascii_whitespace() || b == b'>' || b == b'/' {
                    break;
                }
                i += utf8_len(b);
            }
            (i, AttributeKind::NameValue)
        }
    }
}

/// Skip ASCII whitespace from `from` up to `end`, returning the first
/// non-whitespace offset (or `end`).
fn skip_ws(bytes: &[u8], from: usize, end: usize) -> usize {
    let mut i = from;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Whether an unterminated opaque region of `kind` is one of the hard
/// parse-failure triggers (Requirement 7.1). Comments, CDATA, and raw-text
/// element bodies fail hard when unterminated; declarations, processing
/// instructions, and frontmatter are treated leniently.
fn is_hard_failure_region(kind: OpaqueRegion) -> bool {
    matches!(
        kind,
        OpaqueRegion::Comment | OpaqueRegion::CData | OpaqueRegion::RawTextElement
    )
}

/// The parse-failure message for an unterminated opaque region of `kind`.
fn unterminated_region_message(kind: OpaqueRegion) -> &'static str {
    match kind {
        OpaqueRegion::Comment => "unterminated comment at end of input",
        OpaqueRegion::CData => "unterminated CDATA section at end of input",
        OpaqueRegion::RawTextElement => {
            "unterminated raw-text element body at end of input"
        }
        // These are never passed here (see `is_hard_failure_region`), but keep
        // the match total and panic-free.
        OpaqueRegion::Declaration
        | OpaqueRegion::ProcessingInstruction
        | OpaqueRegion::Frontmatter => "unterminated region at end of input",
    }
}

/// Whether the character at byte offset `at` may begin a tag name under
/// `profile`'s tag-name-start rules. Returns `false` at or past end of input.
fn is_name_start_at(src: &str, at: usize, profile: &ScannerProfile) -> bool {
    match src[at.min(src.len())..].chars().next() {
        Some(ch) if at <= src.len() => profile.tag_name_start.accepts_first_char(ch),
        _ => false,
    }
}

/// A coarse classification of the most recent *significant* (non-whitespace,
/// non-comment) token the JSX/TSX scan walk has passed at top-level child
/// position, used to decide whether a `<` begins a JSX element `Start_Tag` or is
/// a TypeScript `<` (generic argument list / less-than operator).
///
/// This is deliberately **not** a nesting counter — there is no increment /
/// decrement that can drift across fragments (`<>`…`</>`), self-closing tags, or
/// generics the gate declined to rewrite. It is a pure "what did I last see",
/// recomputed continuously as the cursor advances, so it cannot accumulate error
/// across the file (Requirement 2.5). [`scan_start_tags`] maintains a
/// `LastToken` value across the walk and feeds it to [`is_start_tag_position`]
/// and the child-container skip gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastToken {
    /// Start of input, or no significant token seen yet. A `<` here begins a
    /// JSX element (expression position).
    None,
    /// The last significant token produces a value: an identifier character, a
    /// closing `)` or `]`, or a string / number / template literal. A `<`
    /// immediately after a value producer is a TypeScript `<` (generic argument
    /// list or less-than), never a JSX element.
    Ident,
    /// The last significant token introduces an expression: one of
    /// `= ( , { : ? & | ! ; < > } => return yield await do else` and friends. A
    /// `<` here begins a JSX element (expression position).
    OperatorOrOpen,
    /// The walk just passed the `>` that closes a recognized JSX start/opening
    /// tag or a `</name>` end tag. Child position follows, which is expression
    /// position for a nested element, so a following `<` begins a JSX element.
    TagClose,
}

/// Whether a `<` at byte offset `lt_offset` begins a JSX element `Start_Tag`,
/// given the classification `last_significant` of the most recent significant
/// token before it.
///
/// Returns `true` iff **both**:
///
/// 1. the character after the `<` is a dialect-admitted tag-name start
///    ([`is_name_start_at`] at `lt_offset + 1`), **and**
/// 2. the `<` is in expression/value position — `last_significant` is
///    [`LastToken::None`], [`LastToken::OperatorOrOpen`], or
///    [`LastToken::TagClose`], and **not** [`LastToken::Ident`].
///
/// This encodes the design's decision table (Change 2): JSX grammar only admits
/// an element where an expression may begin, so a `<` immediately following a
/// value producer (identifier, `)`, `]`, literal) is a TypeScript generic /
/// less-than and must not be delimited as a tag. `Array<`, `Record<`, `Omit<`,
/// `useState<`, and `axios.get<` all have `last_significant == Ident` and are
/// correctly rejected; `return <div>`, `=> <Item/>`, `{ <Child/> }`, and a
/// `>`-followed `<li>` are correctly accepted (Requirements 2.1, 2.3, 2.5, 3.3).
fn is_start_tag_position(
    src: &str,
    lt_offset: usize,
    last_significant: LastToken,
    profile: &ScannerProfile,
) -> bool {
    if !is_name_start_at(src, lt_offset + 1, profile) {
        return false;
    }
    // The expression-position gate is a JSX/TSX-only concern: TypeScript generic
    // argument lists and `<` operators exist only in JSX/TSX files, where a `<`
    // immediately after a value producer is a generic, never an element. In the
    // markup dialects (HTML/Vue/Astro/Svelte) a `<` after text is a perfectly
    // ordinary adjacent start tag (`<a>text<b>`), so there the first-char rule
    // alone decides — the `last_significant` context is not consulted. The
    // JSX/TSX profile is exactly the set that skips child-expression containers,
    // so that flag doubles as the "apply the gate" marker.
    if !profile.scan_child_expression_containers {
        return true;
    }
    match last_significant {
        LastToken::None | LastToken::OperatorOrOpen | LastToken::TagClose => true,
        LastToken::Ident => false,
    }
}

/// Recompute the running [`LastToken`] classification for an ordinary byte `b`
/// consumed at top-level (child) position by [`scan_start_tags`].
///
/// Returns `Some(classification)` for a significant byte, or `None` for a
/// non-significant byte (whitespace) that must leave the previous classification
/// in force. Strings, template literals, and `<` are handled by the caller (they
/// are consumed as whole tokens); this helper classifies every other byte:
///
/// - **Value producers → [`LastToken::Ident`]**: identifier characters
///   (ASCII alphanumerics, `_`, `$`) and the closers `)` / `]`, which yield a
///   value, so a following `<` is a TypeScript generic / less-than, never a JSX
///   element. Non-ASCII bytes are treated as identifier content (value
///   producers) to stay conservative.
/// - **Expression introducers → [`LastToken::OperatorOrOpen`]**: operators and
///   openers (`= ( , { : ? & | ! ; < > } . / * + - % ^ ~`), after which a `<`
///   begins a JSX element. A bare `>` operator maps here; the tag-closing `>` of
///   a recognized tag is classified as [`LastToken::TagClose`] by the caller,
///   which takes precedence.
fn classify_byte(b: u8) -> Option<LastToken> {
    if b.is_ascii_whitespace() {
        return None;
    }
    // Value producers: identifier chars and the value-yielding closers.
    if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b')' | b']') {
        return Some(LastToken::Ident);
    }
    // Non-ASCII bytes are part of an identifier / value in practice; be
    // conservative and treat them as value producers.
    if !b.is_ascii() {
        return Some(LastToken::Ident);
    }
    // Everything else at top level is an operator or opener — an expression
    // introducer position for a following `<`.
    Some(LastToken::OperatorOrOpen)
}

/// Whether the ASCII byte `b` may begin a JavaScript/TypeScript identifier or
/// keyword (an ASCII letter, `_`, or `$`). Digits are intentionally excluded:
/// a run beginning with a digit is a numeric literal, classified by the
/// per-byte [`classify_byte`] path as a value producer.
fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || matches!(b, b'_' | b'$')
}

/// Scan an identifier / keyword run starting at `from` (a byte for which
/// [`is_ident_start_byte`] holds), returning the offset one past the last
/// identifier byte. Identifier continuation bytes are ASCII alphanumerics, `_`,
/// and `$`.
fn scan_ident_run(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$') {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Whether `word` is a JavaScript/TypeScript keyword that introduces an
/// expression — after which a `<` begins a JSX element rather than a generic /
/// less-than. These are the value-position introducers from the design's
/// [`LastToken::OperatorOrOpen`] set (`return`, `yield`, `await`, `do`, `else`)
/// plus the operator-like keywords that likewise precede an expression
/// (`typeof`, `void`, `delete`, `in`, `of`, `instanceof`, `new`, `case`). Any
/// other identifier is a value producer ([`LastToken::Ident`]).
fn is_expression_keyword(word: &str) -> bool {
    matches!(
        word,
        "return"
            | "yield"
            | "await"
            | "do"
            | "else"
            | "typeof"
            | "void"
            | "delete"
            | "in"
            | "of"
            | "instanceof"
            | "new"
            | "case"
    )
}

/// Consume a whole template literal body starting at `from` (the first byte
/// after the opening `` ` ``), returning the offset one past the closing
/// backtick, or `None` if it is unterminated before `end`.
///
/// Unlike [`scan_template_literal`], which stops at the first `${` boundary,
/// this walks the entire literal including any `${ ... }` interpolations (which
/// may themselves contain nested braces, strings, and template literals), so the
/// top-level walk never descends into interpolation text and a `<` inside a
/// template literal is never mistaken for a tag. It reuses [`scan_balanced`] for
/// each interpolation's brace region.
fn scan_template_literal_whole(src: &str, from: usize, end: usize) -> Option<usize> {
    let mut i = from;
    loop {
        match scan_template_literal(src, i, end) {
            TemplateScan::Closed(next) => return Some(next.min(end)),
            TemplateScan::Interpolation(next) => {
                // `next` is one past the `{` of `${`; `scan_balanced` expects the
                // offset of the opening `{` itself, so start at `next - 1`.
                let brace_open = next.saturating_sub(1);
                let after = scan_balanced(src, brace_open, b'{', b'}')?;
                i = after.max(next);
            }
            TemplateScan::Unterminated => return None,
        }
    }
}

/// Delimit the start tag that opens at the `<` at byte offset `open` and build
/// its [`ScannedTag`], or return a [`ParseFailure`] if the tag is unterminated
/// (or contains an unterminated quoted value or braced expression) at end of
/// input.
fn delimit_start_tag<'a>(
    src: &'a str,
    open: usize,
    profile: &ScannerProfile,
    indent_width: usize,
) -> Result<ScannedTag<'a>, ParseFailure> {
    let bytes = src.as_bytes();
    // The element name starts right after `<` and runs across name characters
    // (letters, digits, and the member/namespace joiners `.`, `:`, `-`, `_`,
    // `$`). The first char was already validated by `is_name_start_at`.
    let name_start = open + 1;
    let name_end = scan_name(src, name_start);
    let name = &src[name_start..name_end];

    // Walk the attribute region to the matching `>`, tracking lexical state so a
    // `>` inside a quoted value or a balanced `{...}`/`(...)` region does not
    // close the tag.
    let mut i = name_end;
    let mut self_closing = false;
    // Whether a `<` was encountered in the attribute region before the tag
    // terminator. A `<` here means the element name is immediately (or later)
    // followed by a TypeScript generic argument list on a component head, as in
    // `<EditableList<SearchEngineDomain>>` (Change 3, Requirement 2.4). The
    // hand-rolled scanner does not parse TS type arguments, so it cannot
    // delimit such a tag with confidence; it records the span but flags the tag
    // not-confidently-rewritable, mirroring the `ComplexExpression` poison, so
    // the tag is emitted byte-for-byte unchanged as a per-tag no-op
    // (Requirement 4.9) rather than having its inner `<` dropped.
    let mut in_attr_region_angle = false;
    loop {
        if i >= bytes.len() {
            return Err(ParseFailure::at(
                src,
                open,
                "unterminated start tag at end of input",
            ));
        }
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i = scan_quoted(src, i + 1, quote).ok_or_else(|| {
                    ParseFailure::at(src, i, "unterminated quoted attribute value at end of input")
                })?;
            }
            b'{' => {
                i = scan_balanced(src, i, b'{', b'}').ok_or_else(|| {
                    ParseFailure::at(src, i, "unterminated braced expression at end of input")
                })?;
            }
            b'(' => {
                // Parens outside braces are unusual in a start tag, but balance
                // them defensively so a `>` inside `(...)` does not close early.
                i = scan_balanced(src, i, b'(', b')').unwrap_or(bytes.len());
            }
            b'<' => {
                // A `<` in the attribute region (before the terminator) is the
                // TypeScript generic argument opener of a component-with-generics
                // head. Mark the tag not-confidently-rewritable and keep walking
                // to the tag terminator so the span is recorded and the main
                // walk resumes correctly after it; the rewrite stage emits the
                // original bytes verbatim (Change 3, Requirements 2.4, 4.9).
                in_attr_region_angle = true;
                i += 1;
            }
            b'/' => {
                // `/>` marks a self-closing tag. A lone `/` elsewhere just moves
                // on; the terminator decision is made when we see `>`.
                if bytes.get(i + 1) == Some(&b'>') {
                    self_closing = true;
                    i += 2;
                    break;
                }
                i += 1;
            }
            b'>' => {
                i += 1;
                break;
            }
            _ => i += utf8_len(bytes[i]),
        }
    }

    let span = open..i;
    let (open_col, base_indent_col) = column_geometry(src, open, indent_width);

    let attributes = tokenize_attributes(src, name_end..span.end, profile, indent_width);
    // Confidence is a property of the whole tag: a single
    // `Complex_Expression_Attribute`, or a `<` in the attribute region (a
    // component-with-generics head, Change 3), poisons it, so the tag is emitted
    // byte-for-byte unchanged by the wrap/render stages (Requirements 2.4, 4.9).
    let confidently_rewritable = !in_attr_region_angle && attributes_confident(&attributes);

    // Probe each confidently-delimited braced attribute value for genuine nested
    // JSX elements and record them as source-relative children (task 4.2,
    // Requirements 4.10, 3.2). This does NOT change `span`, the attribute `raw`
    // slices, or `confidently_rewritable`: the element-bearing attribute still
    // tokenizes as an ordinary `NameValue` whose `raw` is preserved byte-for-byte
    // (task 4.3 relies on this). Only JSX/TSX dialects — the ones that gate `<`
    // on expression position — probe; other dialects always get an empty
    // `children`. A not-confidently-rewritable enclosing tag records no children:
    // it is already a per-tag no-op, so descent would be wasted work.
    let children = if confidently_rewritable && profile.scan_child_expression_containers {
        probe_children(src, open, &attributes, profile, indent_width)?
    } else {
        Vec::new()
    };

    Ok(ScannedTag {
        span: span.clone(),
        name,
        self_closing,
        attributes,
        confidently_rewritable,
        open_col,
        base_indent_col,
        children,
    })
}

/// Byte offset of the subslice `inner` within the backing buffer `src`.
///
/// `inner` must be a subslice of `src` (as every [`Attribute::raw`] produced by
/// [`tokenize_attributes`] is — it is always `&src[..]`), so the pointer
/// difference is a valid in-bounds byte offset on a UTF-8 char boundary. This is
/// how the probe recovers an attribute's absolute source offset without
/// threading offsets through [`tokenize_attributes`].
fn subslice_offset(src: &str, inner: &str) -> usize {
    (inner.as_ptr() as usize).saturating_sub(src.as_ptr() as usize)
}

/// For a name-value attribute `raw` of the form `name = { … }`, return the byte
/// offset of the value's opening `{` within `raw`; otherwise `None`.
///
/// Returns `None` for a quoted (`name="…"`) or unquoted (`name=val`) value even
/// when a `{` appears inside it, so a brace buried in a string is never mistaken
/// for the value's opening brace. The value's opening `{` is the first
/// non-whitespace byte after the first `=` — it must be exactly `{`.
fn braced_value_open_offset(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    let eq = bytes.iter().position(|&b| b == b'=')?;
    let mut j = eq + 1;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    (bytes.get(j) == Some(&b'{')).then_some(j)
}

/// Probe the confidently-delimited braced attribute values of a start tag that
/// opens at `open` for genuine nested JSX elements, returning them as source
/// coordinate children RELATIVE to `open`, each tagged with the owning
/// attribute's source index and the child's offset within that attribute's
/// `raw` slice (task 4.2, Requirements 4.10, 3.2, 7.4).
///
/// Only name-value attributes whose value is a braced `{ … }` expression are
/// probed; plain, quoted, unquoted, pure-spread (`{...props}`,
/// [`AttributeKind::DynamicSpread`]), and not-confidently-delimited
/// ([`AttributeKind::ComplexExpression`]) attributes are never probed and their
/// bytes are preserved verbatim. A `<` inside a braced value that is a
/// TypeScript generic / `<` operator (not in start-tag position) records no
/// child and is skipped exactly as the main walk skips it, so the generics guard
/// holds inside attribute values.
fn probe_children<'a>(
    src: &'a str,
    open: usize,
    attributes: &[Attribute<'a>],
    profile: &ScannerProfile,
    indent_width: usize,
) -> Result<Vec<NestedChild<'a>>, ParseFailure> {
    let mut children: Vec<NestedChild<'a>> = Vec::new();
    for (attr_index, attr) in attributes.iter().enumerate() {
        // Only name-value attributes carry a braced value worth probing. A pure
        // spread has `name_key == None` and kind `DynamicSpread`; a complex
        // expression already poisoned confidence and is excluded by the caller.
        if attr.kind != AttributeKind::NameValue {
            continue;
        }
        // Only a genuine braced VALUE is probed: the attribute must be the
        // name-value form `name = { … }` whose value begins with `{`. A quoted
        // or unquoted value is skipped even if a `{` appears inside it (e.g.
        // `label="a{b}"`), so a brace inside a string is never mistaken for the
        // value's opening brace.
        let attr_abs = subslice_offset(src, attr.raw);
        let Some(rel_brace) = braced_value_open_offset(attr.raw) else {
            continue;
        };
        let brace_open = attr_abs + rel_brace;
        // The attribute `raw` ends exactly at the matching `}` for a braced
        // value (the tokenizer captured `name…value` through the delimited
        // close), so the interior to walk is `[brace_open + 1, brace_end - 1)`
        // where `brace_end` is one past the attribute's last byte.
        let brace_end = attr_abs + attr.raw.len();
        probe_braced_interior(
            src,
            open,
            attr_index,
            attr_abs,
            brace_open,
            brace_end,
            profile,
            indent_width,
            &mut children,
        )?;
    }
    Ok(children)
}

/// Walk the interior of one braced attribute value `[brace_open, brace_end)`
/// (where `brace_open` indexes the opening `{` and `brace_end` is one past the
/// matching `}`), recording each genuine nested JSX element as a
/// [`NestedChild`] relative to the enclosing tag's `open`.
///
/// The walk mirrors the main [`scan_start_tags`] loop's JSX discipline over the
/// interior: it maintains a running [`LastToken`] (the first significant token
/// inside the brace is in expression-introducer position, so an interior `<` is
/// classified exactly as the main walk classifies it), reuses
/// [`step_in_container`] / [`ContainerFrame`] for child-position descendants, and
/// consumes strings / template literals / regex literals / comments /
/// identifiers as whole tokens so a `<` inside any of them is never mistaken for
/// a tag. A `<` that [`is_start_tag_position`] accepts triggers a recursive
/// [`delimit_start_tag`] whose absolute span is converted to be relative to
/// `open` before being pushed; scanning then resumes just past the child, and a
/// non-self-closing child's element body is walked under the ordinary
/// child-position container discipline so descendants in child position are
/// recognized too.
#[allow(clippy::too_many_arguments)]
fn probe_braced_interior<'a>(
    src: &'a str,
    open: usize,
    attr_index: usize,
    attr_abs: usize,
    brace_open: usize,
    brace_end: usize,
    profile: &ScannerProfile,
    indent_width: usize,
    children: &mut Vec<NestedChild<'a>>,
) -> Result<(), ParseFailure> {
    let bytes = src.as_bytes();
    // Interior runs from just after `{` to just before the matching `}`.
    let start = brace_open + 1;
    let end = brace_end.min(bytes.len()).saturating_sub(1).max(start);
    let mut i = start;
    // The first significant token inside a brace is in expression-introducer
    // position, so a leading `<` is a genuine JSX element — seed `OperatorOrOpen`.
    let mut last_significant = LastToken::OperatorOrOpen;
    let mut container_stack: Vec<ContainerFrame> = Vec::new();
    let mut regex_allowed = true;

    while i < end {
        // Inside a child-position `{ … }` container opened within this interior,
        // mirror the container discipline so an interior `}` buried in a string /
        // template / regex / comment, or inside a nested bracket, never closes it
        // early (reuses the same helper the main walk uses).
        if profile.scan_child_expression_containers
            && bytes[i] == b'{'
            && matches!(last_significant, LastToken::None | LastToken::TagClose)
        {
            container_stack.push(ContainerFrame {
                open_at: i,
                interior_depth: 0,
            });
            i += 1;
            last_significant = LastToken::OperatorOrOpen;
            regex_allowed = true;
            continue;
        }
        if !container_stack.is_empty() {
            match step_in_container(src, i, regex_allowed, &mut container_stack)? {
                InteriorStep::Advance { next, last, regex } => {
                    i = next.max(i + 1);
                    last_significant = last;
                    regex_allowed = regex;
                    continue;
                }
                InteriorStep::Fallthrough => {}
            }
        }

        if bytes[i] == b'<' {
            if is_start_tag_position(src, i, last_significant, profile) {
                let child = delimit_start_tag(src, i, profile, indent_width)?;
                let child_end = child.span.end;
                // Record the child with a span RELATIVE to the enclosing tag's
                // `open`, plus the owning attribute's source index and the
                // child's offset within that attribute's `raw` (both source
                // coordinates) so task 4.4 can map source→rendered by attribute
                // identity without any text search (Finding 3).
                let rel_span = (child.span.start - open)..(child.span.end - open);
                let offset_in_attr_raw = child.span.start - attr_abs;
                children.push(NestedChild {
                    owner_attr_index: attr_index,
                    offset_in_attr_raw,
                    tag: ScannedTag {
                        span: rel_span,
                        ..child
                    },
                });
                // Resume just past the child. A recognized opening/self-closing
                // tag yields child position, so a following `<` is a nested
                // element; a following `/` inside a container is division.
                i = child_end.max(i + 1);
                last_significant = LastToken::TagClose;
                regex_allowed = false;
                continue;
            }
            // A `<` not in start-tag position is a TypeScript generic / `<`
            // operator: record no child and skip it as ordinary bytes, exactly
            // as the main walk does. A following `/` (a `</…>` closing-tag slash)
            // must not be read as a regex start.
            i += 1;
            last_significant = LastToken::OperatorOrOpen;
            regex_allowed = false;
            continue;
        }

        // Ordinary (non-`<`, non-skipped) byte: consume it and recompute the
        // running classification exactly as the main walk does.
        let step = step_ordinary_probe_byte(src, i, end, last_significant, regex_allowed)?;
        i = step.next.max(i + 1);
        last_significant = step.last;
        regex_allowed = step.regex;
    }

    Ok(())
}

/// The result of consuming one ordinary (non-`<`, non-container) byte in a
/// braced attribute-value interior walk (see [`step_ordinary_probe_byte`]).
struct OrdinaryStep {
    /// Byte offset to resume from (the caller clamps to at least `i + 1`).
    next: usize,
    /// Updated running token classification.
    last: LastToken,
    /// Whether a `/` at `next` may begin a regex literal.
    regex: bool,
}

/// Consume one ordinary byte at `i` inside a braced attribute-value interior and
/// return the updated walk state, mirroring [`delimit_braced`]'s lexical
/// discipline: strings and template literals are consumed whole (a `<` inside
/// them is never a tag), regex literals and comments are skipped whole, keywords
/// that introduce an expression are classified as such, and every other byte is
/// classified via [`classify_byte`].
///
/// Returns a [`ParseFailure`] only for an unterminated block comment at end of
/// input (Requirement 7.1), located at its opener.
fn step_ordinary_probe_byte(
    src: &str,
    i: usize,
    end: usize,
    last_significant: LastToken,
    regex_allowed: bool,
) -> Result<OrdinaryStep, ParseFailure> {
    let bytes = src.as_bytes();
    let value = |next: usize| OrdinaryStep {
        next,
        last: LastToken::Ident,
        regex: false,
    };
    let operator = |next: usize, regex: bool| OrdinaryStep {
        next,
        last: LastToken::OperatorOrOpen,
        regex,
    };

    let b = bytes[i];
    let step = match b {
        b'\'' | b'"' => match scan_js_string(src, i + 1, b, end) {
            Some(next) => value(next),
            // Unterminated string: treat the quote as an ordinary byte.
            None => operator(i + 1, regex_allowed),
        },
        b'`' => match scan_template_literal_whole(src, i + 1, end) {
            Some(next) => value(next),
            None => operator(i + 1, regex_allowed),
        },
        // Line comment: skip whole; a newline permits a regex after.
        b'/' if bytes.get(i + 1) == Some(&b'/') => {
            operator(skip_line_comment(bytes, i + 2, end), true)
        }
        // Block comment: skip whole; unterminated at EOF is a hard failure.
        b'/' if bytes.get(i + 1) == Some(&b'*') => {
            let Some(next) = skip_block_comment(bytes, i + 2, end) else {
                return Err(ParseFailure::at(
                    src,
                    i,
                    "unterminated braced expression at end of input",
                ));
            };
            operator(next, false)
        }
        // Regex literal where a `/` may start one: skip whole. An unterminated
        // regex is treated conservatively as division.
        b'/' if regex_allowed => match scan_regex_literal(src, i + 1, end) {
            Some(next) => value(next),
            None => operator(i + 1, true),
        },
        // Division operator; an operator means a regex may follow.
        b'/' => operator(i + 1, true),
        _ if is_ident_start_byte(b) => {
            let word_end = scan_ident_run(bytes, i);
            if is_expression_keyword(&src[i..word_end]) {
                operator(word_end, true)
            } else {
                value(word_end)
            }
        }
        _ => {
            let (last, regex) = match classify_byte(b) {
                Some(classified) => (classified, classified == LastToken::OperatorOrOpen),
                // Whitespace / non-significant: leave BOTH the classification and
                // the regex context unchanged (transparent), exactly as the main
                // walk and `delimit_braced` do.
                None => (last_significant, regex_allowed),
            };
            OrdinaryStep {
                next: i + utf8_len(b),
                last,
                regex,
            }
        }
    };
    Ok(step)
}

/// A start tag's element name runs across ASCII alphanumerics and the joiner
/// characters used by member/namespace and framework names (`.`, `:`, `-`, `_`,
/// `$`). Returns the byte offset one past the last name byte.
fn scan_name(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_' | b'$') {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Scan a quoted value starting at `from` (the first byte after the opening
/// quote), returning the offset one past the closing `quote`, or `None` if the
/// quote is never closed before end of input.
fn scan_quoted(src: &str, from: usize, quote: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == quote {
            return Some(i + 1);
        }
        i += utf8_len(bytes[i]);
    }
    None
}

/// Scan a balanced region that opens with `open_b` at offset `from`, returning
/// the offset one past the matching `close_b`, or `None` if unterminated. Nested
/// `open_b`/`close_b` pairs and quoted strings inside are respected so a `}` (or
/// `)`) inside a string does not prematurely close the region.
fn scan_balanced(src: &str, from: usize, open_b: u8, close_b: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                let quote = bytes[i];
                i = scan_quoted(src, i + 1, quote)?;
            }
            b if b == open_b => {
                depth += 1;
                i += 1;
            }
            b if b == close_b => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b => i += utf8_len(b),
        }
    }
    None
}

/// Delimit a JSX/TSX braced attribute region that opens with `{` at `from`,
/// returning `(offset-one-past-the-matching-`}`, confident)` bounded by `end`.
///
/// This is the scanner's confidence model for braced expressions (Requirements
/// 4.9, 7.4). It is deliberately **not** a full JavaScript parser, but it tracks
/// enough lexical state to delimit the common cases with certainty:
///
/// - string literals in all three quote forms — `'...'`, `"..."`, and template
///   literals `` `...` `` — with backslash escapes inside `'`/`"` strings,
/// - template-literal `${ ... }` interpolations, which may nest arbitrarily and
///   contain their own strings, braces, and nested template literals,
/// - regular-expression literals (`/.../flags`), disambiguated from the division
///   operator by the preceding significant token, including `[...]` character
///   classes (where `/` is literal) and backslash escapes,
/// - line (`// ...`) and block (`/* ... */`) comments,
/// - nested braces `{}`, parentheses `()`, and brackets `[]`.
///
/// The returned flag is `true` only when the opener was matched to its closer
/// with the lexical stack fully unwound and no ambiguity encountered. When the
/// lexer cannot be certain — an unterminated string / comment / regex, a bracket
/// mismatch, or running off `end` before the stack empties — it returns
/// `confident = false` together with a **best-effort** end from naive
/// balanced-brace scanning (or `end`), so the caller still has a span to capture
/// verbatim while knowing the tag must be left untouched.
///
/// `from` must index a `{` byte; callers only ever pass a confirmed brace.
fn delimit_braced(src: &str, from: usize, end: usize) -> (usize, bool) {
    // A pessimistic fallback span used whenever confidence is lost: the naive
    // balanced-brace end (which respects simple strings) clamped to `end`, or
    // `end` itself when even that cannot close.
    let fallback = || scan_balanced(src, from, b'{', b'}').unwrap_or(end).min(end);

    let bytes = src.as_bytes();
    if from >= end || bytes[from] != b'{' {
        return (fallback(), false);
    }

    // Bracket-nesting stack. Each entry is the closer byte we expect next. The
    // leading `{` seeds the stack; delimitation succeeds exactly when the stack
    // empties on the matching `}`.
    let mut stack: Vec<u8> = vec![b'}'];
    // Stack of template-literal interpolation depths: when we enter a `${`, we
    // remember the bracket-stack length to restore template-string scanning once
    // the matching `}` closes the interpolation.
    let mut template_interp: Vec<usize> = Vec::new();

    let mut i = from + 1;
    // Tracks whether the previous significant byte permits a regex literal to
    // begin at the current `/` (true right after operators / openers / at the
    // start), versus a division operator (false right after a value / closer).
    let mut regex_allowed = true;

    while i < end {
        let b = bytes[i];
        match b {
            // ---- strings -------------------------------------------------
            b'\'' | b'"' => match scan_js_string(src, i + 1, b, end) {
                Some(next) => {
                    i = next;
                    regex_allowed = false;
                }
                None => return (fallback(), false),
            },
            b'`' => match scan_template_literal(src, i + 1, end) {
                TemplateScan::Closed(next) => {
                    i = next;
                    regex_allowed = false;
                }
                TemplateScan::Interpolation(next) => {
                    // Entered a `${` interpolation: push a brace closer and
                    // remember we must resume template scanning when it closes.
                    template_interp.push(stack.len());
                    stack.push(b'}');
                    i = next;
                    regex_allowed = true;
                }
                TemplateScan::Unterminated => return (fallback(), false),
            },
            // ---- comments ------------------------------------------------
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = skip_line_comment(bytes, i + 2, end);
                // A line comment does not change the value/operator context in a
                // way that matters within a start tag; treat the newline as an
                // operator boundary so a following `/` could begin a regex.
                regex_allowed = true;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                match skip_block_comment(bytes, i + 2, end) {
                    Some(next) => i = next,
                    None => return (fallback(), false),
                }
            }
            // ---- JSX closing-tag slash ----------------------------------
            // A `/` IMMEDIATELY preceded by a `<` is the slash of a JSX closing
            // tag (`</name>`), never a regex-literal start. Without this guard
            // an element-bearing attribute value whose inner element carries an
            // end tag (`primary={ <a>…</a> }`) leaves `regex_allowed == true` at
            // the `<` of `</a>` (a `<` is an expression introducer, classified
            // as regex-permitting by `byte_allows_regex_after`), so the `/`
            // would enter the `scan_regex_literal` arm below, run off to end of
            // input, and report the value not-confident — poisoning the
            // enclosing tag into a no-op. Mirror the identical guard in the main
            // `scan_start_tags` walk and treat the `/` as division. Pure index
            // arithmetic (`i > from && bytes[i-1] == b'<'`) cannot read out of
            // bounds. This only ever SUPPRESSES a spurious regex read after `<`;
            // a `/` not preceded by `<` is unaffected, so values with no `</`
            // delimit to a byte-identical span.
            b'/' if i > from && bytes[i - 1] == b'<' => {
                i += 1;
                regex_allowed = false;
            }
            // ---- regex vs division --------------------------------------
            b'/' if regex_allowed => match scan_regex_literal(src, i + 1, end) {
                Some(next) => {
                    i = next;
                    regex_allowed = false;
                }
                None => return (fallback(), false),
            },
            b'/' => {
                // Division operator; an operator means a regex may follow.
                i += 1;
                regex_allowed = true;
            }
            // ---- bracket nesting ----------------------------------------
            b'{' | b'(' | b'[' => {
                stack.push(match b {
                    b'{' => b'}',
                    b'(' => b')',
                    _ => b']',
                });
                i += 1;
                regex_allowed = true;
            }
            b'}' | b')' | b']' => {
                match stack.pop() {
                    Some(expected) if expected == b => {}
                    // A mismatched closer means our tracking diverged from the
                    // true structure: we cannot delimit with confidence.
                    _ => return (fallback(), false),
                }
                i += 1;
                if stack.is_empty() {
                    // The opening `{` is closed with the stack fully unwound:
                    // a confident delimitation.
                    return (i, true);
                }
                // If this `}` closed a template-literal interpolation, resume
                // scanning the enclosing template string.
                if template_interp.last() == Some(&stack.len()) {
                    template_interp.pop();
                    match scan_template_literal(src, i, end) {
                        TemplateScan::Closed(next) => {
                            i = next;
                            regex_allowed = false;
                        }
                        TemplateScan::Interpolation(next) => {
                            template_interp.push(stack.len());
                            stack.push(b'}');
                            i = next;
                            regex_allowed = true;
                        }
                        TemplateScan::Unterminated => return (fallback(), false),
                    }
                } else {
                    // A closer yields a value, so a following `/` is division.
                    regex_allowed = false;
                }
            }
            // ---- whitespace: transparent --------------------------------
            // Whitespace does not change the value/operator context, so it must
            // NOT flip `regex_allowed`. Otherwise `total / count` would read the
            // `/` after the space as a regex start rather than division.
            _ if b.is_ascii_whitespace() => i += 1,
            // ---- ordinary bytes -----------------------------------------
            _ => {
                // Classify whether this byte leaves a regex permissible after
                // it. Identifiers / literals / `)` `]` produce values (regex not
                // allowed); operators and most punctuation allow a regex next.
                regex_allowed = byte_allows_regex_after(b);
                i += utf8_len(b);
            }
        }
    }

    // Ran off `end` before the stack emptied: not confident.
    (fallback(), false)
}

/// Scan a `'`/`"` JS string body starting at `from` (first byte after the
/// opening quote `q`), honoring backslash escapes. Returns the offset one past
/// the closing quote, or `None` if it is unterminated before `end`.
fn scan_js_string(src: &str, from: usize, q: u8, end: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < end {
        match bytes[i] {
            b'\\' => i += 2, // skip the escaped byte
            b if b == q => return Some(i + 1),
            b => i += utf8_len(b),
        }
    }
    None
}

/// The outcome of scanning a template-literal body up to its next boundary.
enum TemplateScan {
    /// The template literal closed with its backtick; value is one-past-`` ` ``.
    Closed(usize),
    /// A `${` interpolation opened; value is one-past-the `{` of `${`.
    Interpolation(usize),
    /// The template literal ran unterminated to `end`.
    Unterminated,
}

/// Scan a template-literal body starting at `from` (first byte after the opening
/// `` ` `` or after a resumed `}`), stopping at the closing backtick or at a
/// `${` interpolation opener, honoring backslash escapes.
fn scan_template_literal(src: &str, from: usize, end: usize) -> TemplateScan {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < end {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => return TemplateScan::Closed(i + 1),
            b'$' if bytes.get(i + 1) == Some(&b'{') => {
                return TemplateScan::Interpolation(i + 2);
            }
            b => i += utf8_len(b),
        }
    }
    TemplateScan::Unterminated
}

/// Skip a `// ...` line comment starting at `from` (first byte after `//`),
/// returning the offset of the terminating newline (or `end`). The newline
/// itself is left for the caller to re-examine.
fn skip_line_comment(bytes: &[u8], from: usize, end: usize) -> usize {
    let mut i = from;
    while i < end && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Skip a `/* ... */` block comment starting at `from` (first byte after `/*`),
/// returning the offset one past the closing `*/`, or `None` if unterminated
/// before `end`.
fn skip_block_comment(bytes: &[u8], from: usize, end: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < end {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Scan a regex literal body starting at `from` (first byte after the opening
/// `/`), returning the offset one past the trailing flags, or `None` if the
/// literal is unterminated before `end`. Honors `[...]` character classes (where
/// `/` is literal) and backslash escapes, then consumes the ASCII-alphabetic
/// flag run.
fn scan_regex_literal(src: &str, from: usize, end: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = from;
    let mut in_class = false;
    while i < end {
        match bytes[i] {
            b'\\' => i += 2,
            b'[' => {
                in_class = true;
                i += 1;
            }
            b']' if in_class => {
                in_class = false;
                i += 1;
            }
            b'\n' => return None, // regex literals do not span lines
            b'/' if !in_class => {
                // Past the closing `/`: consume trailing flag letters.
                i += 1;
                while i < end && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                return Some(i);
            }
            b => i += utf8_len(b),
        }
    }
    None
}

/// Whether a regular-expression literal may begin immediately after the ASCII
/// byte `b` (used to disambiguate `/` as regex-start versus division). A value
/// producer — identifier char, digit, or a closing `)`/`]` — means the next `/`
/// is division; operators and other punctuation allow a regex to start.
fn byte_allows_regex_after(b: u8) -> bool {
    if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b')' | b']') {
        return false;
    }
    // Non-ASCII bytes are part of an identifier or value in practice; be
    // conservative and treat them as value producers (regex not allowed).
    if !b.is_ascii() {
        return false;
    }
    true
}

/// Public re-export of [`column_geometry`] for the engine's child-geometry
/// recompute (task 4.4, Finding 4).
///
/// After the enclosing tag is wrapped, a nested child's source column no longer
/// reflects where its `<` lands in the rendered output. The engine therefore
/// recomputes the child's `(open_col, base_indent_col)` from the RENDERED buffer
/// at the child's rendered `<` offset, using **exactly** this shared
/// tab = `indent_width` / [`crate::model::display_width`] model so the geometry
/// agrees with how the scanner measured every other tag and so the two
/// idempotence passes read the identical column from the identical buffer
/// position (Requirement 8.5).
#[must_use]
pub fn column_geometry_at(src: &str, open: usize, indent_width: usize) -> (usize, usize) {
    column_geometry(src, open, indent_width)
}

/// Compute `(open_col, base_indent_col)` for a start tag whose `<` is at byte
/// offset `open`, under the tab = `indent_width` column model shared with width
/// budgeting.
///
/// `open_col` is the display column of `<` from the start of its physical line;
/// `base_indent_col` is the display column of the first non-whitespace character
/// of that same line (the line's own indentation). For a tag that begins its
/// line the two are equal; for a mid-line tag `base_indent_col < open_col`.
fn column_geometry(src: &str, open: usize, indent_width: usize) -> (usize, usize) {
    let line_start = src[..open].rfind('\n').map_or(0, |nl| nl + 1);
    let line_prefix = &src[line_start..open];

    // open_col: advance one column per space / display-width per char, and to
    // the next indent_width multiple per tab.
    let open_col = advance_columns(line_prefix, indent_width);

    // base_indent_col: the display column at the first non-whitespace character
    // of the physical line (scanning the whole line, which may extend past the
    // `<` when the tag sits on indentation-only... but indentation precedes any
    // content, so measuring the leading whitespace run of the line suffices).
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |nl| line_start + nl);
    let full_line = &src[line_start..line_end];
    let indent_run_len = full_line
        .char_indices()
        .find(|&(_, c)| !c.is_whitespace())
        .map_or(full_line.len(), |(idx, _)| idx);
    let base_indent_col = advance_columns(&full_line[..indent_run_len], indent_width);

    (open_col, base_indent_col)
}

/// Advance display columns across `text` under the tab = `indent_width` column
/// model: each tab advances to the next `indent_width` multiple, every other
/// code point advances by its display width (via the shared width helper).
fn advance_columns(text: &str, indent_width: usize) -> usize {
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\t' {
            // Advance to the next multiple of indent_width (at least one column).
            if indent_width == 0 {
                col += 1;
            } else {
                col += indent_width - (col % indent_width);
            }
        } else {
            col += crate::model::char_width(ch, indent_width);
        }
    }
    col
}

/// Byte length of the UTF-8 code point whose leading byte is `b`. Continuation
/// and invalid lead bytes map to 1 so the walk always makes progress and stays
/// panic-free on unexpected bytes.
fn utf8_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        // ASCII, continuation, and invalid lead bytes advance by one byte.
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;

    fn html() -> ScannerProfile {
        Dialect::Html.scanner_profile()
    }

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    fn astro() -> ScannerProfile {
        Dialect::Astro.scanner_profile()
    }

    /// Probe at an offset expected to begin an opaque region of `kind`, asserting
    /// the match metadata and returning it for further checks.
    fn expect_match(src: &str, at: usize, profile: &ScannerProfile, kind: OpaqueRegion) -> OpaqueMatch {
        let m = opaque_region_at(src, at, profile)
            .unwrap_or_else(|| panic!("expected an opaque match at {at} in {src:?}"));
        assert_eq!(m.kind, kind, "unexpected opaque kind in {src:?}");
        assert_eq!(m.start, at);
        assert!(m.end <= src.len(), "end {} must be <= len {}", m.end, src.len());
        m
    }

    #[test]
    fn no_region_at_plain_text() {
        let src = "hello <div>";
        assert_eq!(opaque_region_at(src, 0, &html()), None);
        // The `<` of a real start tag is not an opaque region opener.
        assert_eq!(opaque_region_at(src, 6, &html()), None);
    }

    #[test]
    fn out_of_bounds_offset_yields_none() {
        let src = "<!-- x -->";
        assert_eq!(opaque_region_at(src, src.len(), &html()), None);
        assert_eq!(opaque_region_at(src, src.len() + 5, &html()), None);
    }

    #[test]
    fn comment_terminated_and_unterminated() {
        let src = "<!-- a comment --> tail";
        let m = expect_match(src, 0, &html(), OpaqueRegion::Comment);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], " tail");

        let open = "<!-- never closed";
        let m = expect_match(open, 0, &html(), OpaqueRegion::Comment);
        assert!(!m.terminated);
        assert_eq!(m.end, open.len());
    }

    #[test]
    fn comment_with_inner_angle_brackets_is_opaque() {
        // Angle brackets and even `<div>` inside a comment must not be scanned.
        let src = "<!-- <div class=\"x\"> -->after";
        let m = expect_match(src, 0, &html(), OpaqueRegion::Comment);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "after");
    }

    #[test]
    fn cdata_terminated_and_unterminated() {
        let src = "<![CDATA[ raw <tag> data ]]>rest";
        let m = expect_match(src, 0, &html(), OpaqueRegion::CData);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "rest");

        let open = "<![CDATA[ unclosed";
        let m = expect_match(open, 0, &html(), OpaqueRegion::CData);
        assert!(!m.terminated);
        assert_eq!(m.end, open.len());
    }

    #[test]
    fn declaration_doctype_is_opaque() {
        let src = "<!DOCTYPE html>body";
        let m = expect_match(src, 0, &html(), OpaqueRegion::Declaration);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "body");
    }

    #[test]
    fn declaration_does_not_swallow_comment_or_cdata() {
        // `<!--` must classify as a comment, not a declaration, regardless of the
        // profile ordering guard.
        let src = "<!-- c -->x";
        let m = expect_match(src, 0, &html(), OpaqueRegion::Comment);
        assert_eq!(&src[m.end..], "x");

        let src = "<![CDATA[d]]>x";
        let m = expect_match(src, 0, &html(), OpaqueRegion::CData);
        assert_eq!(&src[m.end..], "x");
    }

    #[test]
    fn processing_instruction_is_opaque() {
        let src = "<?xml version=\"1.0\"?>rest";
        let m = expect_match(src, 0, &html(), OpaqueRegion::ProcessingInstruction);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "rest");

        let open = "<?php echo";
        let m = expect_match(open, 0, &html(), OpaqueRegion::ProcessingInstruction);
        assert!(!m.terminated);
        assert_eq!(m.end, open.len());
    }

    #[test]
    fn raw_text_script_body_is_opaque_including_inner_tags() {
        // A `<` and a `</span>` inside the script body must be skipped; only the
        // matching `</script>` closes the region.
        let src = "<script>var x = 1 < 2; foo(\"</span>\");</script>tail";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "tail");
    }

    #[test]
    fn raw_text_style_textarea_title_are_opaque() {
        for (src, tail) in [
            ("<style>.a{color:red}</style>x", "x"),
            ("<textarea>a < b </textarea>y", "y"),
            ("<title>My <Page></title>z", "z"),
        ] {
            let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
            assert!(m.terminated, "expected terminated region in {src:?}");
            assert_eq!(&src[m.end..], tail);
        }
    }

    #[test]
    fn raw_text_with_attributes_in_start_tag() {
        let src = "<script type=\"module\" defer>let y = 3;</script>!";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "!");
    }

    #[test]
    fn raw_text_unterminated_body_runs_to_eof() {
        let src = "<script>never closed";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(!m.terminated);
        assert_eq!(m.end, src.len());
    }

    #[test]
    fn raw_text_unterminated_start_tag_runs_to_eof() {
        // `<script` with no `>` at all: no body, region spans to EOF unterminated.
        let src = "<script type=module";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(!m.terminated);
        assert_eq!(m.end, src.len());
    }

    #[test]
    fn similarly_named_element_is_not_raw_text() {
        // `<scripting>` must not be treated as `<script>`.
        let src = "<scripting>text</scripting>";
        assert_eq!(opaque_region_at(src, 0, &html()), None);
        // Nor `<styled>` as `<style>`.
        let src = "<styled>a</styled>";
        assert_eq!(opaque_region_at(src, 0, &html()), None);
    }

    #[test]
    fn raw_text_end_tag_matched_case_insensitively() {
        let src = "<SCRIPT>a</ScRiPt>tail";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "tail");
    }

    #[test]
    fn raw_text_self_closing_like_still_seeks_end_tag() {
        // A `<script/>`-style tag has no real raw-text semantics in HTML, but the
        // recognizer still looks for `</script>`; without one it is unterminated.
        let src = "<script src=\"a.js\"></script>q";
        let m = expect_match(src, 0, &html(), OpaqueRegion::RawTextElement);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "q");
    }

    #[test]
    fn jsx_profile_only_treats_raw_text_as_opaque() {
        // JSX does not treat comments/CDATA/PI/declarations as opaque regions.
        assert_eq!(opaque_region_at("<!-- c -->", 0, &jsx()), None);
        assert_eq!(opaque_region_at("<![CDATA[x]]>", 0, &jsx()), None);
        assert_eq!(opaque_region_at("<?pi?>", 0, &jsx()), None);
        assert_eq!(opaque_region_at("<!DOCTYPE html>", 0, &jsx()), None);
        // But an embedded raw-text element still is.
        let src = "<script>x<y</script>t";
        let m = expect_match(src, 0, &jsx(), OpaqueRegion::RawTextElement);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "t");
    }

    #[test]
    fn astro_frontmatter_only_at_file_start() {
        let src = "---\nconst a = 1;\n---\n<main>";
        let m = expect_match(src, 0, &astro(), OpaqueRegion::Frontmatter);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "<main>");

        // A `---` not at offset 0 is not frontmatter.
        let mid = "x\n---\nnot frontmatter\n---\n";
        assert_eq!(opaque_region_at(mid, 2, &astro()), None);
    }

    #[test]
    fn astro_frontmatter_unterminated_runs_to_eof() {
        let src = "---\nconst a = 1;";
        let m = expect_match(src, 0, &astro(), OpaqueRegion::Frontmatter);
        assert!(!m.terminated);
        assert_eq!(m.end, src.len());
    }

    #[test]
    fn html_profile_has_no_frontmatter() {
        // `---` at file start is not an opaque region for HTML.
        assert_eq!(opaque_region_at("---\nx\n---\n", 0, &html()), None);
    }

    #[test]
    fn adjacent_regions_resume_cleanly() {
        // Two comments back to back: resuming from the first `end` lands exactly
        // on the opener of the second.
        let src = "<!--a--><!--b-->tail";
        let first = expect_match(src, 0, &html(), OpaqueRegion::Comment);
        assert!(first.terminated);
        let second = expect_match(src, first.end, &html(), OpaqueRegion::Comment);
        assert!(second.terminated);
        assert_eq!(&src[second.end..], "tail");
    }

    #[test]
    fn empty_comment_and_cdata() {
        let src = "<!---->x";
        let m = expect_match(src, 0, &html(), OpaqueRegion::Comment);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "x");

        let src = "<![CDATA[]]>y";
        let m = expect_match(src, 0, &html(), OpaqueRegion::CData);
        assert!(m.terminated);
        assert_eq!(&src[m.end..], "y");
    }

    // -----------------------------------------------------------------------
    // scan_start_tags (task 5.2)
    // -----------------------------------------------------------------------

    /// Scan with HTML rules and `indent_width = 2`, asserting success.
    fn scan_html(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &html(), 2).expect("scan should succeed")
    }

    #[test]
    fn scans_a_single_simple_tag() {
        let src = "<div class=\"a\">";
        let tags = scan_html(src);
        assert_eq!(tags.len(), 1);
        let tag = &tags[0];
        assert_eq!(tag.name, "div");
        assert!(!tag.self_closing);
        assert_eq!(tag.span, 0..src.len());
        assert_eq!(&src[tag.span.clone()], "<div class=\"a\">");
        assert_eq!(tag.open_col, 0);
        assert_eq!(tag.base_indent_col, 0);
    }

    #[test]
    fn records_self_closing_tags() {
        // Canonical and spaced forms both report self_closing and span the `/>`.
        for src in ["<br/>", "<br />", "<img src=\"x\"  />"] {
            let tags = scan_html(src);
            assert_eq!(tags.len(), 1, "one tag in {src:?}");
            assert!(tags[0].self_closing, "{src:?} is self-closing");
            assert_eq!(tags[0].span, 0..src.len());
        }
    }

    #[test]
    fn scans_multiple_tags_in_order_skipping_text() {
        let src = "<a>text<b>more<c>";
        let tags = scan_html(src);
        let names: Vec<_> = tags.iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        // Spans are exact and non-overlapping.
        assert_eq!(&src[tags[0].span.clone()], "<a>");
        assert_eq!(&src[tags[1].span.clone()], "<b>");
        assert_eq!(&src[tags[2].span.clone()], "<c>");
    }

    #[test]
    fn gt_inside_quoted_value_does_not_close_tag() {
        let src = "<a title=\"x > y\" href='a>b'>";
        let tags = scan_html(src);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].span, 0..src.len());
        assert_eq!(&src[tags[0].span.clone()], src);
    }

    #[test]
    fn gt_inside_braced_region_does_not_close_tag() {
        // JSX-style braced expression containing `>` and nested braces.
        let src = "<Comp a={x > y} b={ {k: 1} }>";
        let jsx = Dialect::Jsx.scanner_profile();
        let tags = scan_start_tags(src, &jsx, 2).expect("jsx scan succeeds");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "Comp");
        assert_eq!(tags[0].span, 0..src.len());
    }

    #[test]
    fn member_and_namespace_names_are_scanned() {
        let jsx = Dialect::Jsx.scanner_profile();
        let tags = scan_start_tags("<Foo.Bar x />", &jsx, 2).unwrap();
        assert_eq!(tags[0].name, "Foo.Bar");
        assert!(tags[0].self_closing);

        let tags = scan_start_tags("<svg:rect />", &jsx, 2).unwrap();
        assert_eq!(tags[0].name, "svg:rect");
    }

    #[test]
    fn less_than_not_followed_by_name_start_is_not_a_tag() {
        // `<` as a less-than operator (no admitted name-start char after it).
        let jsx = Dialect::Jsx.scanner_profile();
        assert_eq!(scan_start_tags("a < b && c < d", &jsx, 2).unwrap().len(), 0);
        // `< ` with a space, and `<1` are not tags either.
        assert_eq!(scan_html("< div>").len(), 0);
        assert_eq!(scan_html("<1abc>").len(), 0);
    }

    #[test]
    fn tags_inside_opaque_regions_are_not_scanned() {
        // The `<div>` inside the comment and the `<b>` inside the script body
        // must not be located; only the real `<p>` is.
        let src = "<!-- <div> --><p><script>var a = <b>;</script><p>";
        let tags = scan_html(src);
        let names: Vec<_> = tags.iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["p", "p"]);
    }

    #[test]
    fn open_col_and_base_indent_differ_for_mid_line_tags() {
        // A tag that begins its line: both columns equal the indentation.
        let src = "    <div a>";
        let tag = &scan_html(src)[0];
        assert_eq!(tag.open_col, 4);
        assert_eq!(tag.base_indent_col, 4);

        // A mid-expression tag: base_indent is the line indent, open_col is the
        // column of `<` (well past the indent).
        let src = "  const x = <div a b>";
        let tag = &scan_html(src)[0];
        assert_eq!(tag.base_indent_col, 2);
        assert_eq!(tag.open_col, "  const x = ".len());
        assert!(tag.base_indent_col < tag.open_col);
    }

    #[test]
    fn columns_use_tab_indent_width_model() {
        // A leading tab counts as indent_width columns; the `<` on that line sits
        // at column indent_width.
        let src = "\t<div a>";
        let tags = scan_start_tags(src, &html(), 4).unwrap();
        assert_eq!(tags[0].open_col, 4);
        assert_eq!(tags[0].base_indent_col, 4);

        // The same input under indent_width = 2 yields a different column, proving
        // the shared tab = indent_width model is applied.
        let tags = scan_start_tags(src, &html(), 2).unwrap();
        assert_eq!(tags[0].open_col, 2);
    }

    #[test]
    fn columns_measured_from_physical_line_start() {
        // The tag is on the third physical line; columns reset per line.
        let src = "<a>\n<b>\n      <c x>";
        let tags = scan_html(src);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[2].name, "c");
        assert_eq!(tags[2].open_col, 6);
        assert_eq!(tags[2].base_indent_col, 6);
    }

    #[test]
    fn unterminated_start_tag_is_a_parse_failure() {
        let src = "<div class=\"a\"";
        let err = scan_start_tags(src, &html(), 2).unwrap_err();
        assert!(err.message.contains("unterminated start tag"));
        // Located at the `<` (line 1, column 1).
        assert_eq!((err.line, err.column), (1, 1));
    }

    #[test]
    fn unterminated_quoted_value_is_a_parse_failure() {
        let src = "<a href=\"never closed";
        let err = scan_start_tags(src, &html(), 2).unwrap_err();
        assert!(err.message.contains("unterminated quoted"));
        assert_eq!(err.line, 1);
    }

    #[test]
    fn unterminated_braced_expression_is_a_parse_failure() {
        let src = "<Comp a={ x > y ";
        let jsx = Dialect::Jsx.scanner_profile();
        let err = scan_start_tags(src, &jsx, 2).unwrap_err();
        assert!(err.message.contains("unterminated braced"));
    }

    #[test]
    fn unterminated_opaque_regions_are_parse_failures() {
        // Comment, CDATA, and raw-text element bodies fail hard when
        // unterminated at end of input (Requirement 7.1).
        let comment = "<p></p><!-- not closed";
        let err = scan_start_tags(comment, &html(), 2).unwrap_err();
        assert!(err.message.contains("comment"));

        let cdata = "<![CDATA[ unclosed";
        let err = scan_start_tags(cdata, &html(), 2).unwrap_err();
        assert!(err.message.contains("CDATA"));

        let script = "<script>never closed";
        let err = scan_start_tags(script, &html(), 2).unwrap_err();
        assert!(err.message.contains("raw-text"));
    }

    #[test]
    fn unterminated_declaration_and_pi_are_lenient() {
        // Declarations and processing instructions are NOT hard failures when
        // unterminated: the scanner treats them leniently and simply finds no
        // further tags.
        assert_eq!(scan_html("<!DOCTYPE html").len(), 0);
        assert_eq!(scan_html("<?xml version").len(), 0);
    }

    #[test]
    fn parse_failure_reports_in_bounds_line_and_column() {
        // A failure on a later line reports a 1-based, in-bounds location.
        let src = "<ok>\n<ok2>\n    <bad class=\"x";
        let err = scan_start_tags(src, &html(), 2).unwrap_err();
        assert_eq!(err.line, 3);
        // Column points at the unterminated quote (1-based), within the line.
        assert!(err.column >= 1);
        // Reconstruct the offset and confirm it is in-bounds.
        let line_start = src.rfind('\n').unwrap() + 1;
        assert!(line_start + (err.column - 1) <= src.len());
    }

    #[test]
    fn empty_input_scans_to_no_tags() {
        assert_eq!(scan_html("").len(), 0);
        assert_eq!(scan_html("just text, no tags").len(), 0);
    }

    #[test]
    fn unicode_text_between_tags_is_handled() {
        // Wide and multi-byte characters between tags must not break the byte
        // walk or column math.
        let src = "中文 <a>\n  日本語 <b x>";
        let tags = scan_html(src);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[1].name, "b");
        // Line 2: two leading spaces + "日本語 " (3 wide glyphs = 6 cols + 1 space)
        // -> `<` at column 2 + 7 = 9.
        assert_eq!(tags[1].base_indent_col, 2);
        assert_eq!(tags[1].open_col, 2 + 6 + 1);
    }

    // -----------------------------------------------------------------------
    // tokenize_attributes (task 5.3)
    // -----------------------------------------------------------------------

    use crate::model::{display_width, Attribute};

    /// Scan one tag with the given profile and return its attributes.
    fn attrs_of<'a>(src: &'a str, profile: &ScannerProfile) -> Vec<Attribute<'a>> {
        let tags = scan_start_tags(src, profile, 2).expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected exactly one tag in {src:?}");
        tags[0].attributes.clone()
    }

    /// Assert an attribute's kind, `name_key`, and raw slice in one place.
    fn assert_attr(
        attr: &Attribute<'_>,
        kind: AttributeKind,
        name_key: Option<&str>,
        raw: &str,
    ) {
        assert_eq!(attr.kind, kind, "kind of {:?}", attr.raw);
        assert_eq!(
            attr.name_key.as_deref(),
            name_key,
            "name_key of {:?}",
            attr.raw
        );
        assert_eq!(attr.raw, raw, "raw slice");
        assert_eq!(attr.width, display_width(raw, 2), "width of {:?}", attr.raw);
    }

    #[test]
    fn tokenizes_plain_boolean_attributes() {
        let attrs = attrs_of("<input disabled required>", &html());
        assert_eq!(attrs.len(), 2);
        assert_attr(&attrs[0], AttributeKind::Plain, Some("disabled"), "disabled");
        assert_attr(&attrs[1], AttributeKind::Plain, Some("required"), "required");
    }

    #[test]
    fn tokenizes_quoted_name_value_attributes() {
        let attrs = attrs_of("<a class=\"btn\" id='main'>", &html());
        assert_eq!(attrs.len(), 2);
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("class"),
            "class=\"btn\"",
        );
        assert_attr(&attrs[1], AttributeKind::NameValue, Some("id"), "id='main'");
    }

    #[test]
    fn tokenizes_unquoted_html_values() {
        let attrs = attrs_of("<a id=main data-x=42>", &html());
        assert_eq!(attrs.len(), 2);
        assert_attr(&attrs[0], AttributeKind::NameValue, Some("id"), "id=main");
        assert_attr(
            &attrs[1],
            AttributeKind::NameValue,
            Some("data-x"),
            "data-x=42",
        );
    }

    #[test]
    fn name_key_is_lowercased_but_raw_preserves_case() {
        let attrs = attrs_of("<a ClassName=\"X\" DISABLED>", &html());
        assert_eq!(attrs.len(), 2);
        // name_key is lowercased for grouping/dup detection...
        assert_eq!(attrs[0].name_key.as_deref(), Some("classname"));
        assert_eq!(attrs[1].name_key.as_deref(), Some("disabled"));
        // ...but the raw slice preserves the original casing verbatim.
        assert_eq!(attrs[0].raw, "ClassName=\"X\"");
        assert_eq!(attrs[1].raw, "DISABLED");
    }

    #[test]
    fn tokenizes_braced_jsx_values() {
        let attrs = attrs_of("<Comp className={styles.btn} count={1 + 2}>", &jsx());
        assert_eq!(attrs.len(), 2);
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("classname"),
            "className={styles.btn}",
        );
        assert_attr(
            &attrs[1],
            AttributeKind::NameValue,
            Some("count"),
            "count={1 + 2}",
        );
    }

    #[test]
    fn braced_value_interior_is_captured_verbatim_not_parsed() {
        // Nested braces, a `>` operator, and a string with a `}` inside must all
        // be captured verbatim without being parsed as an expression.
        let src = "<Comp a={ {k: 1, v: \"}\"} } b={x > 1}>";
        let attrs = attrs_of(src, &jsx());
        assert_eq!(attrs.len(), 2);
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("a"),
            "a={ {k: 1, v: \"}\"} }",
        );
        assert_attr(&attrs[1], AttributeKind::NameValue, Some("b"), "b={x > 1}");
    }

    #[test]
    fn pure_spread_attributes_have_no_name_key() {
        let attrs = attrs_of("<Comp {...props} {...rest}>", &jsx());
        assert_eq!(attrs.len(), 2);
        assert_attr(&attrs[0], AttributeKind::DynamicSpread, None, "{...props}");
        assert_attr(&attrs[1], AttributeKind::DynamicSpread, None, "{...rest}");
    }

    #[test]
    fn spread_is_captured_verbatim_with_nested_braces() {
        let src = "<Comp {...{ a: 1, b: {c: 2} }}>";
        let attrs = attrs_of(src, &jsx());
        assert_eq!(attrs.len(), 1);
        assert_attr(
            &attrs[0],
            AttributeKind::DynamicSpread,
            None,
            "{...{ a: 1, b: {c: 2} }}",
        );
    }

    #[test]
    fn mixed_plain_value_and_spread_in_order() {
        let src = "<Comp disabled className={x} {...props} id=\"y\">";
        let attrs = attrs_of(src, &jsx());
        assert_eq!(attrs.len(), 4);
        assert_attr(&attrs[0], AttributeKind::Plain, Some("disabled"), "disabled");
        assert_attr(
            &attrs[1],
            AttributeKind::NameValue,
            Some("classname"),
            "className={x}",
        );
        assert_attr(&attrs[2], AttributeKind::DynamicSpread, None, "{...props}");
        assert_attr(&attrs[3], AttributeKind::NameValue, Some("id"), "id=\"y\"");
    }

    #[test]
    fn name_led_dynamic_directives_keep_their_name() {
        // Vue `v-bind="obj"` and `:class="x"` are name-led dynamic directives:
        // they tokenize as name-value with a name_key, not pure spreads.
        let attrs = attrs_of("<div v-bind=\"obj\" :class=\"x\" @click=\"f\">", &vue());
        assert_eq!(attrs.len(), 3);
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("v-bind"),
            "v-bind=\"obj\"",
        );
        assert_attr(
            &attrs[1],
            AttributeKind::NameValue,
            Some(":class"),
            ":class=\"x\"",
        );
        assert_attr(
            &attrs[2],
            AttributeKind::NameValue,
            Some("@click"),
            "@click=\"f\"",
        );
    }

    #[test]
    fn whitespace_around_equals_is_tolerated() {
        let attrs = attrs_of("<a class = \"btn\"  id\t=\t'x'>", &html());
        assert_eq!(attrs.len(), 2);
        // The raw slice spans from the name through the value, preserving the
        // interior whitespace around `=` verbatim.
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("class"),
            "class = \"btn\"",
        );
        assert_attr(&attrs[1], AttributeKind::NameValue, Some("id"), "id\t=\t'x'");
    }

    #[test]
    fn self_closing_tag_terminator_excluded_from_attributes() {
        for src in ["<br disabled/>", "<img src=\"x\" />", "<Comp a={1}/>"] {
            let profile = if src.contains("Comp") { jsx() } else { html() };
            let attrs = attrs_of(src, &profile);
            // The trailing `/>` must never leak into the last attribute's raw.
            for a in &attrs {
                assert!(!a.raw.contains('>'), "terminator leaked into {:?}", a.raw);
                assert!(!a.raw.ends_with('/'), "slash leaked into {:?}", a.raw);
            }
        }
        let attrs = attrs_of("<img src=\"x\" />", &html());
        assert_eq!(attrs.len(), 1);
        assert_attr(&attrs[0], AttributeKind::NameValue, Some("src"), "src=\"x\"");
    }

    #[test]
    fn zero_attribute_tag_yields_no_attributes() {
        assert_eq!(attrs_of("<div>", &html()).len(), 0);
        assert_eq!(attrs_of("<br/>", &html()).len(), 0);
        assert_eq!(attrs_of("<div   >", &html()).len(), 0);
    }

    #[test]
    fn quoted_value_with_embedded_gt_and_spaces() {
        let attrs = attrs_of("<a title=\"x > y z\">", &html());
        assert_eq!(attrs.len(), 1);
        assert_attr(
            &attrs[0],
            AttributeKind::NameValue,
            Some("title"),
            "title=\"x > y z\"",
        );
    }

    #[test]
    fn widths_match_display_width_of_raw() {
        // A wide CJK value contributes its display width, not its byte length.
        let attrs = attrs_of("<a title=\"中文\">", &html());
        assert_eq!(attrs.len(), 1);
        // `title="` (7) + 2 wide glyphs (4) + `"` (1) = 12 columns.
        assert_eq!(attrs[0].width, 12);
        assert_eq!(attrs[0].width, display_width(attrs[0].raw, 2));
    }

    fn vue() -> ScannerProfile {
        Dialect::Vue.scanner_profile()
    }
}

#[cfg(test)]
mod scanner_integration_tests {
    //! End-to-end integration tests for the scanner and tokenizer (task 5.5).
    //!
    //! Where `mod tests` exercises each scanner component in isolation, these
    //! tests drive `scan_start_tags` across whole, realistic documents that
    //! combine every feature at once — opaque regions, self-closing tags,
    //! quoted and braced attribute values, and verbatim spread capture — and
    //! assert the exact line/column of a hard parse failure (Requirements 3.6,
    //! 7.1, 7.4).
    //!
    //! All tests go through `scan_start_tags` and never construct a
    //! `ScannedTag` directly, so they stay decoupled from the struct's exact
    //! field set (which neighbouring tasks may extend).

    use super::{scan_start_tags, ParseFailure};
    use crate::dialect::{Dialect, ScannerProfile};
    use crate::model::{Attribute, AttributeKind, ScannedTag};

    fn html() -> ScannerProfile {
        Dialect::Html.scanner_profile()
    }

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan `src` under `profile` with `indent_width = 2`, expecting success.
    fn scan<'a>(src: &'a str, profile: &ScannerProfile) -> Vec<ScannedTag<'a>> {
        scan_start_tags(src, profile, 2).expect("integration scan should succeed")
    }

    /// The ordered element names of the scanned tags, for concise assertions on
    /// which tags the walk located (and, by omission, which it skipped).
    fn names<'a>(tags: &'a [ScannedTag<'a>]) -> Vec<&'a str> {
        tags.iter().map(|t| t.name).collect()
    }

    /// Find a scanned attribute by its exact raw slice across all tags.
    fn find_attr<'a>(tags: &'a [ScannedTag<'a>], raw: &str) -> &'a Attribute<'a> {
        tags.iter()
            .flat_map(|t| t.attributes.iter())
            .find(|a| a.raw == raw)
            .unwrap_or_else(|| panic!("no attribute with raw {raw:?} was tokenized"))
    }

    #[test]
    fn end_to_end_html_document_combines_all_features() {
        // One HTML document exercising every scanner feature together:
        //   * a doctype declaration and a comment (opaque — must be skipped),
        //   * a `<script>` raw-text body whose inner `<b>` must NOT be scanned,
        //   * quoted attribute values (double and single) containing a `>`,
        //   * a self-closing `<img .../>` tag,
        //   * plain boolean attributes.
        let src = "\
<!DOCTYPE html>
<!-- banner: <div> should be ignored -->
<section class=\"wrap\" data-role='main > panel'>
  <img src=\"a.png\" alt=\"x > y\" loading=\"lazy\" />
  <script>var a = 1 < 2; render(<b>);</script>
  <button disabled type=\"submit\">Go</button>
</section>";

        let tags = scan(src, &html());

        // Only the real start tags are located. The `<div>` inside the comment
        // is skipped; the whole `<script>...</script>` is an opaque region, so
        // neither its start tag nor its inner `<b>` is ever scanned; and the
        // closing `</section>` is an end tag, not a start tag.
        assert_eq!(names(&tags), vec!["section", "img", "button"]);

        // The <img> tag is self-closing; the others are not.
        let img = tags.iter().find(|t| t.name == "img").unwrap();
        assert!(img.self_closing, "<img ... /> is self-closing");
        assert_eq!(&src[img.span.clone()], "<img src=\"a.png\" alt=\"x > y\" loading=\"lazy\" />");

        let section = &tags[0];
        assert!(!section.self_closing, "<section> is not self-closing");

        // A `>` inside a double-quoted value does not close the tag, and the
        // raw slice is captured verbatim.
        assert_attr(find_attr(&tags, "alt=\"x > y\""), AttributeKind::NameValue, Some("alt"));
        // A `>` inside a single-quoted value likewise.
        assert_attr(
            find_attr(&tags, "data-role='main > panel'"),
            AttributeKind::NameValue,
            Some("data-role"),
        );
        // Plain boolean attribute captured as-is.
        assert_attr(find_attr(&tags, "disabled"), AttributeKind::Plain, Some("disabled"));
    }

    #[test]
    fn end_to_end_jsx_document_combines_braced_values_and_spread() {
        // One JSX document exercising braced values, pure spreads, a
        // self-closing component, a raw-text <script> body, and a plain prop —
        // all captured verbatim.
        let src = "\
const tree = (
  <Layout className={cx('a', 'b')} {...layoutProps}>
    <Avatar src={user.avatar} size={48} rounded />
    <Icon name={glyphs['home > base']} {...{ a: 1, b: { c: 2 } }} />
    <script>const q = <x> && y;</script>
  </Layout>
);";

        let tags = scan(src, &jsx());

        // The `<script>...</script>` is an opaque raw-text region even in JSX,
        // so its start tag and its inner `<x>` are never scanned.
        assert_eq!(names(&tags), vec!["Layout", "Avatar", "Icon"]);

        // Self-closing detection on JSX elements.
        let avatar = tags.iter().find(|t| t.name == "Avatar").unwrap();
        assert!(avatar.self_closing);
        let icon = tags.iter().find(|t| t.name == "Icon").unwrap();
        assert!(icon.self_closing);
        let layout = tags.iter().find(|t| t.name == "Layout").unwrap();
        assert!(!layout.self_closing);

        // Braced values are captured verbatim, including the call expression,
        // member access, and a `>` inside a nested string literal.
        assert_attr(
            find_attr(&tags, "className={cx('a', 'b')}"),
            AttributeKind::NameValue,
            Some("classname"),
        );
        assert_attr(
            find_attr(&tags, "src={user.avatar}"),
            AttributeKind::NameValue,
            Some("src"),
        );
        assert_attr(
            find_attr(&tags, "name={glyphs['home > base']}"),
            AttributeKind::NameValue,
            Some("name"),
        );

        // Spreads are captured verbatim with no name_key, including a spread
        // whose interior is an object literal with nested braces.
        assert_attr(find_attr(&tags, "{...layoutProps}"), AttributeKind::DynamicSpread, None);
        assert_attr(
            find_attr(&tags, "{...{ a: 1, b: { c: 2 } }}"),
            AttributeKind::DynamicSpread,
            None,
        );

        // A plain boolean prop alongside the braced ones.
        assert_attr(find_attr(&tags, "rounded"), AttributeKind::Plain, Some("rounded"));
    }

    #[test]
    fn parse_failure_reports_precise_line_and_column() {
        // An unterminated quoted attribute value on the fourth physical line.
        // The failure must point at the opening quote of that value with a
        // 1-based, in-bounds (line, column).
        let src = "\
<header>
  <nav class=\"top\"></nav>
  <main>
    <article id=\"never-closed>";

        let err = scan_start_tags(src, &html(), 2).unwrap_err();
        assert!(
            err.message.contains("unterminated quoted"),
            "message was {:?}",
            err.message
        );

        // Line 4 (1-based). The failure is located at the opening `\"` of the
        // `id="..."` value: `    <article id=` is 16 characters, so the quote
        // is the 17th character and sits at column 17.
        assert_eq!((err.line, err.column), (4, 17));

        // The reported location is always in-bounds: reconstruct the byte offset
        // and confirm it lands within the source.
        let line_start = src.rfind('\n').unwrap() + 1;
        let offset = line_start + (err.column - 1);
        assert!(offset <= src.len(), "offset {offset} must be <= len {}", src.len());
        assert_eq!(src.as_bytes()[offset], b'"', "column points at the opening quote");
    }

    #[test]
    fn unterminated_start_tag_failure_locates_the_open_bracket() {
        // A different hard trigger: a start tag with no closing `>` before EOF.
        // The location is the `<` on its physical line.
        let src = "<ul>\n  <li>ok</li>\n  <li class='x'";
        let err = scan_start_tags(src, &html(), 2).unwrap_err();
        assert!(err.message.contains("unterminated start tag"), "message {:?}", err.message);
        assert_eq!(err.line, 3);
        // `  <li ...` — the `<` is the third character, column 3.
        assert_eq!(err.column, 3);
    }

    /// Assert an attribute's kind and comparison name without depending on the
    /// `Attribute` struct's full field set.
    fn assert_attr(attr: &Attribute<'_>, kind: AttributeKind, name_key: Option<&str>) {
        assert_eq!(attr.kind, kind, "kind of {:?}", attr.raw);
        assert_eq!(attr.name_key.as_deref(), name_key, "name_key of {:?}", attr.raw);
    }

    /// A `ParseFailure` round-trips its fields; a smoke test guarding the
    /// public shape the engine relies on when surfacing a `Format_Error`.
    #[test]
    fn parse_failure_fields_are_public_and_in_bounds() {
        let err: ParseFailure = scan_start_tags("<div class=\"a", &html(), 2).unwrap_err();
        assert!(err.line >= 1 && err.column >= 1);
        assert!(err.message.contains("unterminated"));
    }
}

#[cfg(test)]
mod confidence_tests {
    //! Scanner-confidence model and per-tag graceful degradation (task 5.4).
    //!
    //! These tests cover the JSX/TSX braced-expression confidence model
    //! ([`delimit_braced`] and its lexical helpers) and the per-tag
    //! not-confidently-rewritable flag ([`ScannedTag::confidently_rewritable`],
    //! set from [`AttributeKind::ComplexExpression`]). They live in a separate
    //! module from `mod tests` (task 5.3) and `scanner_integration_tests`
    //! (task 5.5) so they never collide with those modules' edits.

    use super::*;
    use crate::dialect::Dialect;

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan exactly one start tag with the JSX profile.
    fn one_tag(src: &str) -> ScannedTag<'_> {
        let tags = scan_start_tags(src, &jsx(), 2).expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected exactly one tag in {src:?}");
        tags.into_iter().next().unwrap()
    }

    /// Assert that a start tag is confidently rewritable: every attribute was
    /// delimited with certainty and none is a `ComplexExpression`.
    fn assert_confident(src: &str) {
        let tag = one_tag(src);
        assert!(
            tag.confidently_rewritable,
            "expected {src:?} to be confidently rewritable; attrs = {:?}",
            tag.attributes
        );
        assert!(
            tag.attributes
                .iter()
                .all(|a| a.kind != AttributeKind::ComplexExpression),
            "no attribute should be ComplexExpression in {src:?}"
        );
    }

    /// Assert that a start tag is NOT confidently rewritable because at least
    /// one braced attribute is a `ComplexExpression`.
    fn assert_not_confident(src: &str) {
        let tag = one_tag(src);
        assert!(
            !tag.confidently_rewritable,
            "expected {src:?} to be NOT confidently rewritable; attrs = {:?}",
            tag.attributes
        );
        assert!(
            tag.attributes
                .iter()
                .any(|a| a.kind == AttributeKind::ComplexExpression),
            "at least one attribute should be ComplexExpression in {src:?}"
        );
    }

    // ---- confident common cases -----------------------------------------

    #[test]
    fn simple_identifier_and_member_values_are_confident() {
        assert_confident("<Comp a={x} b={styles.btn} c={obj.deep.path} />");
    }

    #[test]
    fn arithmetic_and_comparison_in_braces_is_confident() {
        // A bare `>` inside a braced expression is a comparison operator, not a
        // tag terminator; the tag still delimits confidently.
        assert_confident("<Comp a={1 + 2} b={x > 1 ? a : b} />");
    }

    #[test]
    fn nested_object_and_array_literals_are_confident() {
        assert_confident("<Comp style={{ width: 10, pad: [1, 2, 3] }} />");
        // A string containing a brace must not unbalance the nesting.
        assert_confident("<Comp a={ { k: \"}\" } } />");
    }

    #[test]
    fn all_three_string_forms_are_confident() {
        assert_confident("<Comp a={'single'} b={\"double\"} c={`template`} />");
        // Strings whose contents include braces, slashes, and angle brackets.
        assert_confident("<Comp a={'a/b'} b={\"x > y\"} c={'}{'} />");
    }

    #[test]
    fn template_literal_with_interpolation_is_confident() {
        // `${ ... }` interpolations nest and may contain their own braces and
        // strings; the lexer tracks the template/interp nesting.
        assert_confident("<Comp a={`hi ${name}!`} />");
        assert_confident("<Comp a={`${cond ? `${inner}` : \"x\"} tail`} />");
        assert_confident("<Comp a={`a ${ { k: 1 } } b`} />");
    }

    #[test]
    fn regex_literal_is_confident() {
        // A regex literal may contain `/`, `>`, `}`, and a `[...]` class whose
        // `/` is literal; none of these close the braced region prematurely.
        assert_confident("<Comp a={/ab+c/gi} />");
        // An escaped `/` and a `[...]` class containing `>` and `}` stay inside
        // the regex and do not close the braced region.
        assert_confident("<Comp a={/a\\/b[>}]c/} />");
        // Division after a value is not a regex and still balances.
        assert_confident("<Comp a={total / count} />");
    }

    #[test]
    fn line_and_block_comments_in_braces_are_confident() {
        assert_confident("<Comp a={x /* a } brace in a comment */ + y} />");
        assert_confident("<Comp a={fn(/* ) */ arg)} />");
    }

    #[test]
    fn arrow_function_body_is_confident() {
        assert_confident("<button onClick={() => x > 1 ? a : b} />");
        assert_confident("<Comp onChange={(e) => setValue(e.target.value)} />");
    }

    #[test]
    fn confident_spread_is_dynamic_spread_not_complex() {
        let tag = one_tag("<Comp {...props} {...{ a: 1, b: [2, 3] }} />");
        assert!(tag.confidently_rewritable);
        assert_eq!(tag.attributes[0].kind, AttributeKind::DynamicSpread);
        assert_eq!(tag.attributes[1].kind, AttributeKind::DynamicSpread);
    }

    // ---- uncertain cases: ComplexExpression + per-tag no-op --------------

    #[test]
    fn bracket_mismatch_in_braces_is_complex() {
        // A `)` that does not match the open bracket (here a `[`) means the
        // lexer's structural tracking diverged: confidence is lost even though
        // the tag's `{...}` still balances and the tag delimits fine.
        assert_not_confident("<Comp a={ [1, 2) } b={1} />");
        // A `]` closing an open `(` is likewise a mismatch.
        assert_not_confident("<Comp a={ foo(1, 2] } />");
    }

    #[test]
    fn one_uncertain_attribute_poisons_the_whole_tag() {
        // The first two attributes are perfectly delimitable, but a single
        // uncertain braced attribute marks the ENTIRE tag not-confidently-
        // rewritable (confidence is a per-tag property). The tag itself is a
        // complete, well-terminated start tag.
        let tag = one_tag("<Comp ok={1} fine=\"yes\" bad={ [a, b) } />");
        assert!(!tag.confidently_rewritable);
        // Exactly the offending attribute is the ComplexExpression.
        let complex: Vec<_> = tag
            .attributes
            .iter()
            .filter(|a| a.kind == AttributeKind::ComplexExpression)
            .collect();
        assert_eq!(complex.len(), 1, "only the bad attribute is complex");
        assert_eq!(complex[0].name_key.as_deref(), Some("bad"));
    }

    #[test]
    fn complex_attribute_raw_is_still_captured_verbatim() {
        // Even when confidence is lost, the attribute's raw slice is a best-
        // effort span so the per-tag no-op can emit the tag's original bytes.
        let tag = one_tag("<Comp a={ [1, 2) } b={1} />");
        let complex = tag
            .attributes
            .iter()
            .find(|a| a.kind == AttributeKind::ComplexExpression)
            .expect("one complex attribute");
        // The raw slice spans from the attribute name through the best-effort
        // braced end, so the per-tag no-op has real bytes to emit verbatim.
        // (These checks also imply the slice is non-empty.)
        assert!(complex.raw.starts_with("a={"));
        assert!(complex.raw.ends_with('}'));
    }

    // ---- lexical helper unit tests --------------------------------------

    #[test]
    fn delimit_braced_reports_confident_end() {
        let src = "{ a: 1, b: { c: 2 } }X";
        let (end, confident) = delimit_braced(src, 0, src.len());
        assert!(confident);
        assert_eq!(&src[..end], "{ a: 1, b: { c: 2 } }");
        assert_eq!(&src[end..], "X");
    }

    #[test]
    fn delimit_braced_loses_confidence_on_unterminated() {
        // No closing brace before `end`: not confident, best-effort end at EOF.
        let src = "{ a: 1";
        let (_end, confident) = delimit_braced(src, 0, src.len());
        assert!(!confident);
    }

    // ---- Design Decision 0: the `</` regex guard (task 1.2) --------------
    //
    // These tests pin the behavior of the `</`-regex guard added in task 1.1.
    // Before the guard, an element-bearing attribute value whose inner element
    // carries a closing `</…>` tag delimited `confident == false` (the `<` of
    // the closing tag left `regex_allowed == true`, so the following `/` was
    // read as a regex literal that ran off to end of input), which poisoned the
    // enclosing tag into a `ComplexExpression` no-op. The guard treats a `/`
    // immediately after a `<` as a JSX closing-tag slash, so these values now
    // delimit confidently. Crucially the guard only ever SUPPRESSES a spurious
    // regex read right after `<`, so a genuine regex literal and any value with
    // no `</` at all are unaffected.

    #[test]
    fn delimit_braced_confident_on_end_tag_bearing_inner_element() {
        // `{<a className="x">t</a>}` — the inner `<a>` carries a closing `</a>`.
        // Before the guard this was `confident == false`; now it is confident
        // and the delimited span is the whole braced value.
        let src = r#"{<a className="x">t</a>}X"#;
        let (end, confident) = delimit_braced(src, 0, src.len());
        assert!(confident, "end-tag-bearing inner element must delimit confidently");
        assert_eq!(&src[..end], r#"{<a className="x">t</a>}"#);
        assert_eq!(&src[end..], "X");
    }

    #[test]
    fn delimit_braced_confident_on_nested_end_tag_bearing_elements() {
        // `{<span><Chip/></span>}` — an outer `<span>…</span>` with a nested
        // self-closing `<Chip/>`. The two `</…>` closers (and `/>`) must not be
        // read as regex literals. Before the guard this was `confident == false`.
        let src = "{<span><Chip/></span>}Y";
        let (end, confident) = delimit_braced(src, 0, src.len());
        assert!(confident, "nested end-tag-bearing elements must delimit confidently");
        assert_eq!(&src[..end], "{<span><Chip/></span>}");
        assert_eq!(&src[end..], "Y");
    }

    #[test]
    fn delimit_braced_confident_on_self_closing_inner_element() {
        // `{<a className="x" />}` — a self-closing inner element. This case was
        // ALREADY confident before the guard (no `</…>` closer), so on its own
        // it would give false confidence in the guard; it is included only to
        // show the guard does not regress the already-confident path. The
        // end-tag-bearing cases above are the mandatory ones.
        let src = r#"{<a className="x" />}Z"#;
        let (end, confident) = delimit_braced(src, 0, src.len());
        assert!(confident, "self-closing inner element must delimit confidently");
        assert_eq!(&src[..end], r#"{<a className="x" />}"#);
        assert_eq!(&src[end..], "Z");
    }

    #[test]
    fn delimit_braced_confident_on_genuine_regex_not_preceded_by_lt() {
        // `{re.test(/a}b/)}` — a genuine regex literal `/a}b/` whose `/` is NOT
        // preceded by `<`. The guard only suppresses a regex read immediately
        // after `<`, so this `/` is still read as a regex: the `}` inside the
        // regex does NOT close the brace early, and the value delimits
        // confidently to the outer `}`.
        let src = "{re.test(/a}b/)}tail";
        let (end, confident) = delimit_braced(src, 0, src.len());
        assert!(confident, "a genuine regex literal must still delimit confidently");
        assert_eq!(&src[..end], "{re.test(/a}b/)}");
        assert_eq!(&src[end..], "tail");
    }

    #[test]
    fn delimit_braced_span_unchanged_for_values_without_slash_lt() {
        // Values with no `</` at all — template literals, arrows, ternaries, TS
        // generics, and `<` operators — delimit to exactly the span the
        // pre-guard lexer produced. The guard's condition (`bytes[i-1] == b'<'`)
        // never fires for these, so each resolves to its matching outer `}` with
        // `confident == true`, and TS generics / `<` operators are not misread
        // as tags (no `>` ever closes the brace; only the balanced `}` does).
        let cases = [
            // template literal
            ("{`${a}-${b}`}", "rest"),
            // arrow function
            ("{() => handleRun(id)}", "X"),
            // ternary with arrow
            ("{can ? () => run() : undefined}", "Y"),
            // TS generics: `useState<Array<Task>>()` — the `<`/`>` are generic
            // brackets, never tag delimiters; only the outer `}` closes.
            ("{useState<Array<Task>>()}", "Z"),
            // `<` comparison operator
            ("{a < b}", "W"),
        ];
        for (braced, tail) in cases {
            let src = format!("{braced}{tail}");
            let (end, confident) = delimit_braced(&src, 0, src.len());
            assert!(confident, "value without `</` must delimit confidently: {braced}");
            assert_eq!(&src[..end], braced, "delimited span must equal the braced value: {braced}");
            assert_eq!(&src[end..], tail, "scanning must resume just past the value: {braced}");
        }
    }

    #[test]
    fn scan_js_string_honors_escapes() {
        // The escaped closing quote does not end the string.
        let src = "\"a\\\"b\"rest";
        let next = scan_js_string(src, 1, b'"', src.len()).expect("terminated");
        assert_eq!(&src[..next], "\"a\\\"b\"");
        assert_eq!(&src[next..], "rest");
        // Unterminated string reports None.
        assert_eq!(scan_js_string("\"open", 1, b'"', 5), None);
    }

    #[test]
    fn scan_regex_literal_handles_class_and_flags() {
        let src = "ab[/]c/gi 1";
        let next = scan_regex_literal(src, 0, src.len()).expect("terminated regex");
        // Consumes through the closing `/` and the `gi` flags, stopping at the
        // first non-letter (the space).
        assert_eq!(&src[..next], "ab[/]c/gi");
        assert_eq!(&src[next..], " 1");
        // A newline before the closing `/` means not a valid regex literal.
        assert_eq!(scan_regex_literal("ab\n/", 0, 4), None);
    }

    #[test]
    fn block_comment_skips_interior_braces_and_slashes() {
        let src = "/* } / */tail";
        let next = skip_block_comment(src.as_bytes(), 2, src.len()).expect("closed");
        assert_eq!(&src[next..], "tail");
        assert_eq!(skip_block_comment(b"/* open", 2, 7), None);
    }

    #[test]
    fn non_jsx_dialect_never_yields_complex_expression() {
        // HTML has no braced values, so no attribute is ever ComplexExpression
        // and every scanned tag is confidently rewritable.
        let html = Dialect::Html.scanner_profile();
        let tags = scan_start_tags("<div class=\"a\" id='b' disabled>", &html, 2).unwrap();
        assert_eq!(tags.len(), 1);
        assert!(tags[0].confidently_rewritable);
        assert!(tags[0]
            .attributes
            .iter()
            .all(|a| a.kind != AttributeKind::ComplexExpression));
    }
}

#[cfg(test)]
mod confidence_model_tests {
    //! The two end-to-end confidence outcomes task 5.6 calls out, asserted
    //! against the behaviour the wrap/render stages will key off (Requirements
    //! 4.9, 7.4).
    //!
    //! Where `mod confidence_tests` (task 5.4) exercises the lexical machinery
    //! of [`delimit_braced`] and catalogues many confident / uncertain shapes,
    //! this module focuses narrowly on the two cases the task specifies and
    //! pins the behaviour the downstream pipeline depends on:
    //!
    //! 1. A confidently-delimited JSX start tag — `confidently_rewritable` is
    //!    `true`, no attribute is a [`AttributeKind::ComplexExpression`], so the
    //!    wrap/render stages are free to wrap and sort it. We also confirm its
    //!    `span` slices back to the exact original bytes (the scanner never
    //!    loses or shifts the tag it is about to hand to wrapping).
    //! 2. A start tag carrying a `Complex_Expression_Attribute` — the flag is
    //!    `false`, so wrapping/sorting is gated off and the tag is a per-tag
    //!    no-op. We assert the **byte-for-byte** preservation that no-op relies
    //!    on: the tag's `span`, the poisoning attribute's `raw`, and every other
    //!    attribute's `raw` all slice back to the identical original source
    //!    bytes, so re-emitting the span reproduces the input exactly.
    //!
    //! These assertions are expressed at the scanner level because the wrap and
    //! render stages (task 8) consume exactly these two signals — the
    //! `confidently_rewritable` gate and the verbatim `span` — to decide
    //! between "wrap/sort this tag" and "emit its original bytes unchanged".

    use super::*;
    use crate::dialect::Dialect;

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan exactly one JSX start tag from `src`.
    fn one_tag(src: &str) -> ScannedTag<'_> {
        let tags = scan_start_tags(src, &jsx(), 2).expect("scan should succeed");
        assert_eq!(tags.len(), 1, "expected exactly one tag in {src:?}");
        tags.into_iter().next().unwrap()
    }

    // ---- case (a): a confidently-delimited tag can be wrapped/sorted -----

    #[test]
    fn confident_jsx_tag_is_wrappable_and_sortable() {
        // A start tag whose every attribute (plain, quoted, member-access
        // braced value, arithmetic braced value, and a pure spread) delimits
        // with certainty.
        let src = "<Comp disabled className={styles.btn} count={1 + 2} id=\"x\" {...rest} />";
        let tag = one_tag(src);

        // The gate the wrap/render stages read: confidently rewritable, so the
        // pipeline MAY wrap and sort this tag.
        assert!(
            tag.confidently_rewritable,
            "a fully-delimited tag must be confidently rewritable; attrs = {:?}",
            tag.attributes
        );

        // Every attribute was tokenized as a normal, non-complex kind — there is
        // nothing that forces the per-tag no-op, so wrapping/sorting is unlocked.
        assert!(
            tag.attributes
                .iter()
                .all(|a| a.kind != AttributeKind::ComplexExpression),
            "no attribute may be ComplexExpression on a confident tag; attrs = {:?}",
            tag.attributes
        );
        assert_eq!(tag.attributes.len(), 5, "all five attributes are tokenized");

        // The attributes the sorter will reorder and the packer will reflow are
        // the real, individually-captured tokens (so sorting/wrapping has
        // something to operate on rather than one opaque blob).
        let kinds: Vec<_> = tag.attributes.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            vec![
                AttributeKind::Plain,
                AttributeKind::NameValue,
                AttributeKind::NameValue,
                AttributeKind::NameValue,
                AttributeKind::DynamicSpread,
            ]
        );

        // The scanner handed wrapping a span that slices back to the exact
        // original bytes — the tag it will wrap is the tag that was in source.
        assert_eq!(&src[tag.span.clone()], src);
    }

    // ---- case (b): a Complex_Expression_Attribute tag is a verbatim no-op -

    #[test]
    fn complex_expression_tag_is_a_byte_for_byte_no_op() {
        // A single braced attribute whose structure the scanner cannot delimit
        // with confidence (a `)` closing an open `[`). It sits between two
        // perfectly ordinary attributes.
        let src = "<Comp ok={1} bad={ [a, b) } title=\"t\" />";
        let tag = one_tag(src);

        // The gate is OFF: one uncertain attribute poisons the whole tag, so the
        // wrap/render stages must skip it and emit its original bytes.
        assert!(
            !tag.confidently_rewritable,
            "a tag with a Complex_Expression_Attribute must NOT be confidently rewritable; \
             attrs = {:?}",
            tag.attributes
        );

        // Exactly the offending attribute is the ComplexExpression; the others
        // stayed normal kinds (confidence poisoning is per-tag, not per-attr).
        let complex: Vec<_> = tag
            .attributes
            .iter()
            .filter(|a| a.kind == AttributeKind::ComplexExpression)
            .collect();
        assert_eq!(complex.len(), 1, "only the bad attribute is complex");
        assert_eq!(complex[0].name_key.as_deref(), Some("bad"));

        // Byte-for-byte preservation that the per-tag no-op depends on:
        //
        // 1. The tag's span slices back to the identical original bytes, so
        //    re-emitting `&src[tag.span]` reproduces the input exactly.
        assert_eq!(
            &src[tag.span.clone()],
            src,
            "the skipped tag's span must reproduce the original bytes verbatim"
        );

        // 2. The poisoning attribute's raw slice is the exact original source
        //    for that attribute (never reconstructed), including its interior
        //    whitespace and the mismatched bracket.
        assert_eq!(
            complex[0].raw, "bad={ [a, b) }",
            "the complex attribute's raw bytes are preserved verbatim"
        );

        // 3. Every attribute's raw slice is a genuine substring of the source at
        //    its own offset — proving no attribute's bytes were altered, which
        //    is what makes emitting the whole span a true no-op.
        for attr in &tag.attributes {
            assert!(
                src.contains(attr.raw),
                "attribute raw {:?} must be a verbatim slice of the source",
                attr.raw
            );
        }
    }

    #[test]
    fn complex_expression_tag_preserved_among_sibling_tags() {
        // In a document with a confident tag, a skipped (complex) tag, and
        // another confident tag, the skipped tag's bytes round-trip through its
        // span unchanged while its neighbours remain independently rewritable.
        let src = "<A x={1} /><B bad={ foo(1, 2] } /><C y=\"z\" />";
        let tags = scan_start_tags(src, &jsx(), 2).expect("scan should succeed");
        assert_eq!(tags.len(), 3);

        let (confident, skipped): (Vec<_>, Vec<_>) =
            tags.iter().partition(|t| t.confidently_rewritable);
        assert_eq!(confident.len(), 2, "A and C are confidently rewritable");
        assert_eq!(skipped.len(), 1, "only B is skipped");

        // The skipped tag (B) reproduces its original bytes verbatim via its
        // span — the per-tag no-op leaves it exactly as it was in source.
        let b = skipped[0];
        assert_eq!(b.name, "B");
        assert_eq!(&src[b.span.clone()], "<B bad={ foo(1, 2] } />");

        // The confident neighbours likewise slice back to their own original
        // bytes, so reverting B never disturbs A or C.
        assert_eq!(&src[confident[0].span.clone()], "<A x={1} />");
        assert_eq!(&src[confident[1].span.clone()], "<C y=\"z\" />");
    }

    // ---- task 4.2: element-bearing attribute values record children ------

    /// Resolve a child's ABSOLUTE source span from its enclosing tag: the child
    /// span is stored relative to the enclosing tag's `span.start`, so adding it
    /// back recovers the original buffer offsets. Used to assert the stored
    /// relative span round-trips to the exact child bytes.
    fn child_abs<'a>(src: &'a str, enclosing: &ScannedTag<'_>, child: &NestedChild<'_>) -> &'a str {
        let base = enclosing.span.start;
        &src[base + child.tag.span.start..base + child.tag.span.end]
    }

    #[test]
    fn self_closing_element_in_attribute_value_is_recorded_as_child() {
        // The enclosing tag stays whole + confidently rewritable; the braced
        // `primary={ <Chip ... /> }` tokenizes as an ordinary NameValue and the
        // inner `<Chip/>` is recorded as a single child (Requirements 4.10, 3.2).
        let src = "<ListItemText id={1} primary={ <Chip size={\"small\"} color={c} /> } />";
        let tag = one_tag(src);

        // Enclosing tag unchanged by the probe.
        assert!(tag.confidently_rewritable);
        assert_eq!(&src[tag.span.clone()], src);
        assert_eq!(tag.name, "ListItemText");

        // Exactly one child was recorded.
        assert_eq!(tag.children.len(), 1, "one nested element");
        let child = &tag.children[0];
        assert_eq!(child.tag.name, "Chip");
        assert!(child.tag.self_closing);

        // The relative span round-trips to the exact child start-tag bytes.
        assert_eq!(child_abs(src, &tag, child), "<Chip size={\"small\"} color={c} />");

        // Provenance: the owning attribute is `primary` (source index 1), and
        // the intra-raw offset points at the child's `<` within `primary`'s raw.
        assert_eq!(child.owner_attr_index, 1);
        let owner = &tag.attributes[child.owner_attr_index];
        assert_eq!(owner.name_key.as_deref(), Some("primary"));
        assert!(
            owner.raw[child.offset_in_attr_raw..].starts_with("<Chip"),
            "offset_in_attr_raw must point at the child's `<` within the owner raw: {:?}",
            &owner.raw[child.offset_in_attr_raw..]
        );
    }

    #[test]
    fn end_tag_bearing_element_in_attribute_value_is_recorded() {
        // An inner element that carries a `</…>` end tag (exercises Decision 0 /
        // task 1): the enclosing tag is confident and the inner `<a>` start tag
        // is recorded as a child. `delimit_start_tag` records only the START tag
        // span, so the child span is `<a href="x">` through its `>`.
        let src = "<Item primary={ <a href=\"x\">label</a> } />";
        let tag = one_tag(src);
        assert!(tag.confidently_rewritable);
        assert_eq!(tag.children.len(), 1);
        let child = &tag.children[0];
        assert_eq!(child.tag.name, "a");
        assert!(!child.tag.self_closing);
        assert_eq!(child_abs(src, &tag, child), "<a href=\"x\">");
    }

    #[test]
    fn child_position_descendant_in_attribute_value_is_recorded() {
        // `<Stack ...><IconButton .../></Stack>` inside an attribute value: both
        // the opening `<Stack>` and the child-position `<IconButton/>` are
        // recorded, because after the `<Stack>` `>` the walk resumes at child
        // position and recognizes the adjacent `<IconButton` (no text between).
        let src =
            "<Row action={ <Stack direction={\"row\"}><IconButton onClick={f} /></Stack> } />";
        let tag = one_tag(src);
        assert!(tag.confidently_rewritable);
        let names: Vec<_> = tag.children.iter().map(|c| c.tag.name).collect();
        assert_eq!(names, vec!["Stack", "IconButton"]);
        // Both children belong to the same owning attribute (`action`).
        assert!(tag.children.iter().all(|c| c.owner_attr_index == 0));
        assert_eq!(child_abs(src, &tag, &tag.children[1]), "<IconButton onClick={f} />");
    }

    #[test]
    fn arbitrary_depth_nesting_is_recorded_recursively() {
        // `a={ <X b={ <Y c={ <Z /> } /> } />` — each genuine element nests one
        // level deeper via the child's own `children`, so the tree has depth 3.
        let src = "<T a={ <X b={ <Y c={ <Z /> } /> } /> } />";
        let tag = one_tag(src);
        assert_eq!(tag.children.len(), 1);
        let x = &tag.children[0].tag;
        assert_eq!(x.name, "X");
        assert_eq!(x.children.len(), 1);
        let y = &x.children[0].tag;
        assert_eq!(y.name, "Y");
        assert_eq!(y.children.len(), 1);
        let z = &y.children[0].tag;
        assert_eq!(z.name, "Z");
        assert_eq!(z.children.len(), 0, "deepest element has no further children");
    }

    #[test]
    fn generic_in_attribute_value_records_no_child() {
        // A TypeScript generic / `<` operator inside a braced attribute value is
        // in value position (`Ident`), so it is NOT a start tag: no child is
        // recorded and the enclosing tag stays confident (the generics guard
        // holds inside attribute values).
        for src in [
            "<Comp value={useState<Array<Task>>()} />",
            "<Comp cfg={axios.get<Array<Url>>(u)} />",
            "<Comp disabled={a < b} />",
            "<Comp onClick={() => arr.map<T>(x => x)} />",
        ] {
            let tag = one_tag(src);
            assert!(
                tag.confidently_rewritable,
                "generic value keeps the tag confident: {src:?}"
            );
            assert!(
                tag.children.is_empty(),
                "a generic/`<`-operator records no child: {src:?} -> {:?}",
                tag.children.iter().map(|c| c.tag.name).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn pure_spread_is_never_probed() {
        // `{...props}` is a DynamicSpread with no `name=`, never an attribute
        // value, so it is never probed and records no child — even though a `<`
        // could theoretically appear in a spread expression, the kind gate
        // excludes it. Here there is no element anyway; assert no children.
        let src = "<Comp {...props} id={1} />";
        let tag = one_tag(src);
        assert!(tag.confidently_rewritable);
        assert_eq!(tag.children.len(), 0, "a pure spread is never probed");
    }

    #[test]
    fn non_jsx_dialects_never_record_children() {
        // HTML has no braced attribute values and does not gate `<` on
        // expression position, so `children` is always empty.
        let html = Dialect::Html.scanner_profile();
        let tags = scan_start_tags("<div class=\"a\" id=\"b\">", &html, 2).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].children.len(), 0, "non-JSX dialects never record children");
    }

    #[test]
    fn string_and_template_content_in_attribute_value_is_not_a_child() {
        // A `<` inside a string / template literal in the attribute value must
        // not be mistaken for a tag (consumed as a whole token).
        let src = "<Comp label={\"<not-a-tag>\"} tpl={`a<b ${x}`} />";
        let tag = one_tag(src);
        assert!(tag.confidently_rewritable);
        assert!(
            tag.children.is_empty(),
            "no child from string/template content: {:?}",
            tag.children.iter().map(|c| c.tag.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn quoted_value_containing_a_brace_is_not_probed() {
        // A quoted value whose text contains `{` and a `<` must NOT be probed:
        // the value's opening delimiter is the quote, not the interior `{`, so
        // the `<` inside the string is never mistaken for a nested element.
        let src = "<Comp label=\"a{<b>}c\" id={1} />";
        let tag = one_tag(src);
        assert!(tag.confidently_rewritable);
        assert_eq!(
            tag.children.len(),
            0,
            "a brace inside a quoted value is not a braced value: {:?}",
            tag.children.iter().map(|c| c.tag.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_identical_inner_elements_record_distinct_children_by_identity() {
        // Two identical `<a .../>` in DIFFERENT attribute values: each is keyed
        // to its own owning attribute index + intra-raw offset, so identity is
        // unambiguous even though the `raw` substrings are identical (Finding 3).
        let src = "<Row first={ <a href=\"x\" /> } second={ <a href=\"x\" /> } />";
        let tag = one_tag(src);
        assert_eq!(tag.children.len(), 2);
        assert_eq!(tag.children[0].tag.name, "a");
        assert_eq!(tag.children[1].tag.name, "a");
        // Distinct owning attributes.
        assert_eq!(
            tag.attributes[tag.children[0].owner_attr_index].name_key.as_deref(),
            Some("first")
        );
        assert_eq!(
            tag.attributes[tag.children[1].owner_attr_index].name_key.as_deref(),
            Some("second")
        );
        // Each offset points at its own owner's `<a`.
        for c in &tag.children {
            let owner = &tag.attributes[c.owner_attr_index];
            assert!(owner.raw[c.offset_in_attr_raw..].starts_with("<a "));
        }
    }

    #[test]
    fn not_confidently_rewritable_enclosing_tag_records_no_children() {
        // A genuinely ambiguous braced value (mismatched brackets) poisons the
        // enclosing tag; descent is suppressed, so even a real element in a
        // DIFFERENT attribute value is not recorded (the whole tag is a no-op).
        let src = "<Comp bad={ [a, b) } good={ <Chip x={1} /> } />";
        let tag = one_tag(src);
        assert!(!tag.confidently_rewritable);
        assert!(
            tag.children.is_empty(),
            "a not-confidently-rewritable tag records no children"
        );
    }

    // ---- task 4.3: enclosing-tag invariants + flat top-level resume ------

    #[test]
    fn element_bearing_attribute_value_keeps_enclosing_tag_whole_and_confident() {
        // Case (a): an enclosing tag with an element-bearing, confidently
        // delimited attribute value stays confidently_rewritable and its span
        // runs from the `<` through the closing `/>` — the element-bearing `{`
        // does NOT truncate the span, and the nested element does NOT poison
        // confidence (Requirements 2.5, 2.6, 2.8, 4.9, 4.10). The real-world
        // `<TaskStatisticListItem>` shape is used so the invariant is asserted on
        // the exact bug fixture, not just a toy.
        let src = "<TaskStatisticListItem key={k} index={i} status={s} exists={e} \
primary={ <a className=\"c\" href=\"/x\">label</a> } onRun={r} onDelete={d} />";
        let tag = one_tag(src);

        // Span runs through the closing `/>` — whole tag, untruncated.
        assert_eq!(&src[tag.span.clone()], src, "enclosing span runs through `/>`");
        assert!(src[tag.span.clone()].ends_with("/>"), "span ends at the self-close `/>`");
        assert_eq!(tag.name, "TaskStatisticListItem");
        assert!(tag.self_closing);

        // Confidence is unaffected by the nested element in a confident value.
        assert!(
            tag.confidently_rewritable,
            "a nested element in a confidently-delimited value does not poison confidence"
        );

        // The enclosing tag keeps ALL of its own top-level attributes (key,
        // index, status, exists, primary, onRun, onDelete = seven), and the
        // element-bearing `primary` tokenizes as an ordinary NameValue whose raw
        // is preserved byte-for-byte (the probe never splits it out).
        assert_eq!(tag.attributes.len(), 7, "all seven top-level attributes retained");
        let primary = tag
            .attributes
            .iter()
            .find(|a| a.name_key.as_deref() == Some("primary"))
            .expect("primary attribute present");
        assert_eq!(primary.kind, AttributeKind::NameValue);
        assert_eq!(primary.raw, "primary={ <a className=\"c\" href=\"/x\">label</a> }");

        // The nested `<a>` lives ONLY in children, never as a top-level sibling.
        assert_eq!(tag.children.len(), 1);
        assert_eq!(tag.children[0].tag.name, "a");
    }

    #[test]
    fn scan_produces_flat_non_overlapping_source_ordered_top_level_list() {
        // Case (b): across several sibling TOP-LEVEL tags in genuine JSX child
        // position (adjacent inside a fragment, separated only by transparent
        // whitespace so each `<` stays in expression/child position),
        // `scan_start_tags` yields a FLAT list — nested attribute-value elements
        // are recorded only in `children`, never promoted into the flat list —
        // and the flat spans are non-overlapping and strictly source-ordered,
        // exactly as the engine splice requires. The middle tag carries a nested
        // `<Chip/>` child; the flat list must be
        // `[Fragment-open, Row, ListItemText, Footer]` with NO `Chip`.
        let src = "<>\n\
<Row a={1} />\n\
<ListItemText primary={p} secondary={ <Chip size={\"sm\"} color={c} /> } />\n\
<Footer b={2} />\n\
</>";
        let tags = scan_start_tags(src, &jsx(), 2).expect("scan succeeds");

        // Flat list is exactly the TOP-LEVEL tags in source order; the nested
        // `<Chip/>` is NOT present as a top-level entry. (The fragment opener
        // `<>` has no name-start char, so it is not recorded as a tag.)
        let names: Vec<_> = tags.iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["Row", "ListItemText", "Footer"], "flat top-level list");
        assert!(
            !names.contains(&"Chip"),
            "a nested attribute-value element must NOT appear in the flat list"
        );

        // The nested element is recorded under the middle tag's children only.
        let middle = tags.iter().find(|t| t.name == "ListItemText").unwrap();
        assert_eq!(middle.children.len(), 1);
        assert_eq!(middle.children[0].tag.name, "Chip");

        // Spans are strictly source-ordered and non-overlapping: each tag starts
        // at or after the previous tag's end (the main walk resumed at
        // `tag.span.end` and never re-entered an attribute container).
        for pair in tags.windows(2) {
            assert!(
                pair[0].span.end <= pair[1].span.start,
                "flat spans must be non-overlapping and source-ordered: {:?} then {:?}",
                pair[0].span,
                pair[1].span
            );
        }

        // Each flat span round-trips to its exact element bytes.
        assert_eq!(&src[tags[0].span.clone()], "<Row a={1} />");
        assert_eq!(
            &src[tags[1].span.clone()],
            "<ListItemText primary={p} secondary={ <Chip size={\"sm\"} color={c} /> } />"
        );
        assert_eq!(&src[tags[2].span.clone()], "<Footer b={2} />");
    }

    #[test]
    fn enclosing_component_generics_head_poisons_and_suppresses_descent() {
        // Case (c), enclosing side: an enclosing tag that is itself a
        // component-with-generics head (a `<` in its attribute region) is
        // not-confidently-rewritable, so descent is suppressed — NO child is
        // probed for it and the head itself is a per-tag no-op (Requirements
        // 2.6, 4.9). (The component-generics head is delimited up to its first
        // inner generic `>`, pre-existing Change-3 behavior; what task 4.3 pins
        // down is that such a head NEVER probes children.)
        let src = "<Select<Array<number>> value={v} slot={ <Chip x={1} /> }>";
        let tags = scan_start_tags(src, &jsx(), 2).expect("scan succeeds");
        let head = tags
            .iter()
            .find(|t| t.name == "Select")
            .expect("the component head is recorded");
        assert!(
            !head.confidently_rewritable,
            "a component-with-generics head is not confidently rewritable"
        );
        assert!(
            head.children.is_empty(),
            "descent is suppressed for a not-confidently-rewritable enclosing tag"
        );
    }

    #[test]
    fn nested_component_generics_head_child_carries_its_own_lost_confidence() {
        // Case (c), child side: a confident enclosing tag whose attribute value
        // contains a component-with-generics head records that head as a child,
        // and the CHILD independently carries its own lost confidence (its own
        // `<` in its attribute region poisons only itself, Requirement 2.4/4.9),
        // while the enclosing tag stays confident. Each child carries its own
        // confidence flag.
        let src = "<Row slot={ <EditableList<SearchEngineDomain> a=\"1\" /> } id={2} />";
        let tag = one_tag(src);
        assert!(
            tag.confidently_rewritable,
            "the enclosing tag's own attributes are all confident"
        );
        assert_eq!(tag.children.len(), 1, "the generics-head child is recorded");
        let child = &tag.children[0];
        assert_eq!(child.tag.name, "EditableList");
        assert!(
            !child.tag.confidently_rewritable,
            "the nested component-generics head carries its OWN lost confidence"
        );
    }

    #[test]
    fn genuinely_ambiguous_value_anywhere_in_attr_value_poisons_enclosing_and_suppresses_descent() {
        // Case (c) variant: a genuinely ambiguous braced region (mismatched
        // brackets) appearing ANYWHERE inside an enclosing tag's braced
        // attribute value — even nested within an inner element — makes
        // `delimit_braced` lose confidence for that whole top-level value, which
        // poisons the enclosing tag (ComplexExpression). A
        // not-confidently-rewritable enclosing tag suppresses descent, so NO
        // child is probed: the whole tag is a byte-for-byte no-op (Requirements
        // 2.6, 4.9, 7.4). This is the conservative, safe behavior — the plugin
        // declines to rewrite rather than guess at an ambiguous expression.
        let src = "<Row slot={ <Chip bad={ [a, b) } /> } id={2} />";
        let tag = one_tag(src);
        assert!(
            !tag.confidently_rewritable,
            "an ambiguous region inside a braced value poisons the enclosing tag"
        );
        assert!(
            tag.children.is_empty(),
            "descent is suppressed for a not-confidently-rewritable enclosing tag"
        );
        // The whole tag span is still recorded verbatim so the walk resumes
        // correctly past it.
        assert_eq!(&src[tag.span.clone()], src);
    }
}

#[cfg(test)]
mod preservation_property_tests {
    //! Property 2: non-start-tag content is preserved byte-for-byte (task 5.7).
    //!
    //! **Validates: Requirements 3.6**
    //!
    //! The scanner is the structural foundation of the whole-document
    //! preservation guarantee: everything in the source that lies *outside* the
    //! located start-tag spans is content the formatter copies verbatim — it is
    //! never scanned for tags and never rewritten. These property tests pin that
    //! foundation down directly on `scan_start_tags`: for randomly assembled
    //! documents (interleaving text, comments, end tags, raw-text element bodies,
    //! and well-formed start tags) the located spans must *partition* the buffer
    //! so that the bytes between spans are exact, verbatim slices of the source
    //! and splicing the spans back in reproduces the input byte-for-byte.
    //!
    //! This module lives apart from `mod tests` (task 5.3/5.5 unit tests),
    //! `scanner_integration_tests` (task 5.5), and `confidence_tests` (task 5.4)
    //! so it never collides with those modules.

    use super::*;
    use crate::dialect::{Dialect, ScannerProfile};
    use proptest::prelude::*;

    /// The dialects whose scanner profiles we exercise. Covering HTML (unquoted
    /// values, no braces), JSX (braced values/spreads, member names), and Astro
    /// (frontmatter opaque region) spreads the generated documents across the
    /// meaningfully different scanning rules.
    fn profile_for(idx: usize) -> ScannerProfile {
        match idx % 3 {
            0 => Dialect::Html.scanner_profile(),
            1 => Dialect::Jsx.scanner_profile(),
            _ => Dialect::Astro.scanner_profile(),
        }
    }

    /// A document fragment. Documents are built by concatenating a sequence of
    /// these; the generators keep every fragment *well-formed and terminated* so
    /// the scanner never hits one of the enumerated hard parse-failure triggers
    /// (an unterminated tag / quote / brace / opaque region). Property 20 owns
    /// the failure paths; Property 2 is about the success path's preservation.
    #[derive(Clone, Debug)]
    enum Fragment {
        /// Plain text with no `<`, so it can never be mistaken for a tag opener.
        Text(String),
        /// A terminated HTML comment.
        Comment(String),
        /// A terminated CDATA section.
        CData(String),
        /// An end tag such as `</div>`.
        EndTag(String),
        /// A terminated raw-text element body (`<script>…</script>`), opaque in
        /// its entirety.
        RawText { name: String, body: String },
        /// A well-formed start tag the scanner should locate.
        StartTag(String),
    }

    impl Fragment {
        /// Render the fragment to its source text.
        fn render(&self) -> String {
            match self {
                Fragment::Text(t) => t.clone(),
                Fragment::Comment(c) => format!("<!--{c}-->"),
                Fragment::CData(c) => format!("<![CDATA[{c}]]>"),
                Fragment::EndTag(n) => format!("</{n}>"),
                Fragment::RawText { name, body } => {
                    format!("<{name}>{body}</{name}>")
                }
                Fragment::StartTag(s) => s.clone(),
            }
        }
    }

    /// Text that contains no `<` (so it is pure inter-tag content) and no `-`
    /// runs that could spill out of a comment body. Kept short to keep documents
    /// readable in shrink output.
    ///
    /// `{` / `}` are deliberately excluded: under the JSX/TSX profile a
    /// child-position `{` opens an opaque expression container (bugfix Change 1),
    /// so a bare `{` is no longer inert inter-tag text — an unbalanced `{` would
    /// be an unterminated braced expression (a hard parse failure per Requirement
    /// 7.1), which is Property 20's concern, not this property's. Keeping the
    /// alphabet free of braces keeps every generated run genuinely inert
    /// inter-tag content across all exercised profiles.
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\n'),
                Just('\t'),
                Just('a'),
                Just('Z'),
                Just('='),
                Just('"'),
                Just('é'),
                Just('漢'),
            ],
            0..8,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// Comment/CDATA interior text: no `>` and no run that could form the closing
    /// sentinel, so the region stays terminated exactly where we place its
    /// closer.
    fn opaque_body_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just(' '), Just('x'), Just('\n'), Just('<'), Just('/')],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// An element name: an ASCII-letter lead followed by letters/digits. Shared
    /// by start tags, end tags, and raw-text elements.
    fn name_strategy() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,5}"
    }

    /// A raw-text element name drawn from the opaque set, so the generated
    /// `<name>…</name>` is scanned as a single opaque region, never for tags.
    fn raw_text_name_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("script".to_string()),
            Just("style".to_string()),
            Just("textarea".to_string()),
            Just("title".to_string()),
        ]
    }

    /// A well-formed start tag string: `<name` followed by zero or more plain /
    /// quoted-value attributes and a `>` or ` />` terminator. Every quote is
    /// balanced and every tag is terminated, so the scanner locates exactly this
    /// one tag and nothing leaks into surrounding content.
    fn start_tag_strategy() -> impl Strategy<Value = String> {
        let attr = prop_oneof![
            // Plain boolean attribute.
            "[a-z][a-z0-9]{0,4}",
            // Double-quoted value (interior has no quote so it stays balanced).
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,5}")
                .prop_map(|(n, v)| format!("{n}=\"{v}\"")),
            // Single-quoted value.
            ("[a-z][a-z0-9]{0,4}", "[a-zA-Z0-9 ]{0,5}")
                .prop_map(|(n, v)| format!("{n}='{v}'")),
        ];
        (
            name_strategy(),
            prop::collection::vec(attr, 0..4),
            any::<bool>(),
        )
            .prop_map(|(name, attrs, self_closing)| {
                let mut s = String::from("<");
                s.push_str(&name);
                for a in &attrs {
                    s.push(' ');
                    s.push_str(a);
                }
                if self_closing {
                    s.push_str(" />");
                } else {
                    s.push('>');
                }
                s
            })
    }

    /// A single fragment.
    fn fragment_strategy() -> impl Strategy<Value = Fragment> {
        prop_oneof![
            text_strategy().prop_map(Fragment::Text),
            opaque_body_strategy().prop_map(Fragment::Comment),
            opaque_body_strategy().prop_map(Fragment::CData),
            name_strategy().prop_map(Fragment::EndTag),
            (raw_text_name_strategy(), opaque_body_strategy())
                .prop_map(|(name, body)| Fragment::RawText { name, body }),
            start_tag_strategy().prop_map(Fragment::StartTag),
        ]
    }

    /// A whole document: a sequence of fragments concatenated. Returns both the
    /// rendered source and the fragment list (the latter is handy for debugging
    /// shrink output).
    fn document_strategy() -> impl Strategy<Value = (String, Vec<Fragment>)> {
        prop::collection::vec(fragment_strategy(), 0..10).prop_map(|frags| {
            let src: String = frags.iter().map(Fragment::render).collect();
            (src, frags)
        })
    }

    proptest! {
        /// Property 2 (Validates: Requirements 3.6).
        ///
        /// The located start-tag spans partition the buffer: they are ordered,
        /// non-overlapping, in-bounds, and lie on char boundaries. Splicing each
        /// span back between the verbatim inter-span regions reconstructs the
        /// source byte-for-byte, which is exactly the statement that everything
        /// outside the start tags is preserved verbatim.
        #[test]
        fn located_spans_partition_the_buffer(
            (src, _frags) in document_strategy(),
            profile_idx in 0usize..3,
        ) {
            let profile = profile_for(profile_idx);
            // Generated documents are all well-formed, so scanning succeeds.
            let tags = match scan_start_tags(&src, &profile, 2) {
                Ok(tags) => tags,
                // A hard parse failure is out of scope for Property 2 (it is
                // Property 20's concern). Our generators avoid the enumerated
                // triggers, so this should not fire; if it ever does, surface it.
                Err(e) => {
                    prop_assert!(false, "unexpected parse failure in {src:?}: {e:?}");
                    unreachable!()
                }
            };

            // Reconstruct the source by walking the spans in order: emit the
            // verbatim bytes before each span, then the span itself.
            let mut cursor = 0usize;
            let mut rebuilt = String::with_capacity(src.len());
            for tag in &tags {
                let span = tag.span.clone();
                // Spans are in-bounds and ordered/non-overlapping.
                prop_assert!(span.start >= cursor, "spans out of order or overlapping in {src:?}");
                prop_assert!(span.end <= src.len(), "span end out of bounds in {src:?}");
                prop_assert!(span.start <= span.end, "inverted span in {src:?}");
                // Spans lie on UTF-8 boundaries so slicing is always valid.
                prop_assert!(src.is_char_boundary(span.start));
                prop_assert!(src.is_char_boundary(span.end));

                // The inter-span region is verbatim source — append it unchanged.
                rebuilt.push_str(&src[cursor..span.start]);
                // The span is the located tag; append it unchanged too.
                rebuilt.push_str(&src[span.clone()]);
                cursor = span.end;
            }
            // Trailing content after the last span is also verbatim.
            rebuilt.push_str(&src[cursor..]);

            // Splicing spans back into the inter-span text reproduces the input
            // exactly: nothing outside the spans was altered, dropped, or moved.
            prop_assert_eq!(&rebuilt, &src);
        }

        /// Property 2, content facet (Validates: Requirements 3.6).
        ///
        /// The concatenation of every byte region lying *outside* the located
        /// start-tag spans equals the source with exactly the span bytes removed,
        /// and each such region is a verbatim slice of the original source. This
        /// is the "non-start-tag content is preserved byte-for-byte" statement in
        /// its most direct form.
        #[test]
        fn outside_regions_are_verbatim_source(
            (src, _frags) in document_strategy(),
            profile_idx in 0usize..3,
        ) {
            let profile = profile_for(profile_idx);
            let tags = scan_start_tags(&src, &profile, 2)
                .unwrap_or_else(|e| panic!("unexpected parse failure in {src:?}: {e:?}"));

            // Build the "outside" text two independent ways and require they
            // agree with each other and with a direct reference computation.
            //
            // (1) by concatenating inter-span slices of the source:
            let mut outside_from_slices = String::new();
            let mut cursor = 0usize;
            for tag in &tags {
                outside_from_slices.push_str(&src[cursor..tag.span.start]);
                cursor = tag.span.end;
            }
            outside_from_slices.push_str(&src[cursor..]);

            // (2) a reference: start from the whole source and blank out exactly
            // the span byte ranges, keeping all other bytes in place.
            let mut keep = vec![true; src.len()];
            for tag in &tags {
                for b in tag.span.clone() {
                    keep[b] = false;
                }
            }
            let reference: String = src
                .char_indices()
                .filter(|&(i, _)| keep[i])
                .map(|(_, c)| c)
                .collect();

            prop_assert_eq!(&outside_from_slices, &reference,
                "inter-span slices disagree with reference removal in {:?}", src);
        }
    }
}

#[cfg(test)]
mod generics_bug_exploration {
    //! Bugfix exploration tests (bugfix spec task 1, FLIPPED by task 7): TSX/JSX
    //! generics misparsed as JSX start tags.
    //!
    //! **Task 7 has flipped every assertion in this module from the UNFIXED
    //! corruption it originally captured to the corrected (`F'`) behavior**,
    //! re-using the exact same constructs from task 1 (no parallel construct set
    //! is introduced here). Each test now asserts that the fix closes the root
    //! cause it was written to pin:
    //!
    //! 1. *Child-container skip* (Change 1) — child-position `{ … }` containers
    //!    are now opaque in the JSX/TSX profile, so generics / `<` operators
    //!    inside JSX children are no longer scanned for tags: no tag is produced
    //!    and every `<`/`>` is preserved.
    //! 2. *Expression-position gate* (Change 2) — a `<` in `Ident`/value-producer
    //!    position is classified as a TypeScript generic opener and left alone,
    //!    so `useState<`, `Record<`, `Omit<`, `axios.get<`, and the mixed
    //!    statement+JSX cascade's trailing generic are all preserved verbatim
    //!    while genuine components still wrap.
    //! 3. *Component-with-generics head* (Change 3) — `<EditableList<T>>` is a
    //!    not-confidently-rewritable per-tag no-op, left byte-for-byte unchanged.
    //! 4. *Self-check blind spot* (Change 4) — the symmetric-misread corrupting
    //!    pair now FAILS the angle-bracket-conservation invariant (so the engine
    //!    reverts/errors and never emits the changed-invalid output), while the
    //!    superseded `fingerprints_equivalent`-only comparison is still shown to
    //!    judge that pair "equivalent" — the regression-pin the new invariant
    //!    closes.
    //!
    //! Each test documents, inline, the EXPECTED (`F'`) behavior and the history
    //! of the corruption it supersedes. Where the input is valid TypeScript the
    //! output is asserted byte-for-byte unchanged (identical markup structure,
    //! Requirement 7.5).
    //!
    //! Every full-pass observation drives the crate-internal `engine::format_once`
    //! on a `.tsx` path with a deliberately permissive configuration (fluid
    //! wrapping on) so that a genuine tag is actually rewritten — making it
    //! observable that a *generic* is, by contrast, left completely untouched.

    use super::{scan_start_tags, ScannedTag};
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::{Dialect, ScannerProfile};
    use crate::engine::format_once;
    use std::path::Path;

    /// The JSX/TSX scanner profile under test.
    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// A `.tsx`-oriented configuration: fluid wrapping on, sorting off, all
    /// dialects enabled so any extension resolves, `indent_width = 2`. Fluid
    /// wrapping being on is what makes a (mis)recognized tag get rewritten.
    fn tsx_config() -> Configuration {
        Configuration {
            line_width: 80,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Scan `src` with the JSX profile, asserting the scan itself succeeds (the
    /// bug is a *misread*, not a parse failure).
    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("exploration inputs scan without a parse failure")
    }

    /// The ordered element names the scanner located.
    fn names<'a>(tags: &'a [ScannedTag<'a>]) -> Vec<&'a str> {
        tags.iter().map(|t| t.name).collect()
    }

    /// Run a single `format_once` pass over a `.tsx` input and return the
    /// candidate bytes as a string. The exploration inputs never hit a hard
    /// error, so success is asserted.
    fn format_tsx(src: &str) -> String {
        let out = format_once(Path::new("file.tsx"), src.as_bytes(), None, &tsx_config())
            .expect("exploration inputs format without an engine error");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    /// Count the `<` and `>` characters over the whole buffer — the quantities
    /// the bugfix's angle-bracket-conservation invariant (Change 4) will later
    /// compare. Used here only to *document* that the corruption drops a bracket.
    fn angle_counts(s: &str) -> (usize, usize) {
        (s.matches('<').count(), s.matches('>').count())
    }

    // The self-check blind-spot case (task 7) reconstructs the symmetric-misread
    // document fingerprints directly and exercises both the superseded
    // fingerprint comparison and the new angle-bracket-conservation invariant.
    use crate::model::{Fingerprint, FingerprintItem, TagFingerprint};
    use crate::verify::{angle_brackets_conserved, fingerprints_equivalent};

    // ----- Root cause 2: statement/expression-position generics -----------

    #[test]
    fn statement_position_generic_is_misread_and_drops_a_bracket() {
        // FLIPPED EARLY (bugfix task 4.2 / Change 2). The original exploration
        // assertion captured the UNFIXED misread: `useState<` opened a bogus tag
        // named `Array` and a full pass dropped a `<`. Change 2's drift-proof
        // expression-position gate lands in THIS task, so the `<` after the
        // identifier `useState` (an `Ident`/value-producer position) is now
        // correctly classified as a TypeScript generic opener and left alone.
        // That makes the old (buggy) assertion impossible to keep, so it is
        // flipped here to the now-correct behavior rather than waiting for
        // task 7 (Requirements 2.1, 2.3).
        //   EXPECTED (F'): byte-for-byte unchanged, both `<` preserved, no tag.
        let src = "const [t] = useState<Array<Task>>([])";

        // No tag is delimited from the generic head: `useState<`, and the nested
        // `Array<`, are each in `Ident` position and rejected by the gate.
        let tags = scan(src);
        assert!(
            tags.is_empty(),
            "FIXED (Change 2): the statement-position generic yields no tag, got: {:?}",
            names(&tags)
        );

        // A full pass leaves the generic byte-for-byte unchanged: both `<` and
        // both `>` are preserved.
        let out = format_tsx(src);
        assert_eq!(out, src, "FIXED (Change 2): the generic is preserved verbatim");
        assert_eq!(
            angle_counts(src),
            angle_counts(&out),
            "FIXED (Change 2): no `<`/`>` is dropped"
        );
    }

    // ----- Root cause 2: multi-argument (comma-separated) generics --------

    #[test]
    fn multi_arg_generic_record_reflows_args_as_attributes() {
        // FLIPPED EARLY (bugfix task 4.2 / Change 2): `Record<subjectKey, string>`.
        // The UNFIXED scanner delimited `<subjectKey, string>` as a tag and
        // dropped the comma separator. Change 2's gate rejects the `<` after the
        // identifier `Record` (value-producer position), so the generic is left
        // intact and no tag is produced. Flipped here because landing Change 2
        // contradicts the old assertion (Requirement 2.3).
        //   EXPECTED (F'): unchanged, including the `,` separator.
        let src = "const r = Record<subjectKey, string>";

        let tags = scan(src);
        assert!(
            tags.is_empty(),
            "FIXED (Change 2): the multi-arg generic yields no tag, got: {:?}",
            names(&tags)
        );

        let out = format_tsx(src);
        assert_eq!(out, src, "FIXED (Change 2): the generic is preserved verbatim");
        assert!(out.contains(','), "FIXED (Change 2): the comma separator is preserved");
        assert_eq!(angle_counts(src), angle_counts(&out), "brackets conserved");
    }

    #[test]
    fn multi_arg_generic_omit_reflows_args_as_attributes() {
        // FLIPPED EARLY (bugfix task 4.2 / Change 2): `Omit<T, id>`. The UNFIXED
        // scanner delimited `<T, id>` as a tag named `T` and dropped the comma.
        // Change 2's gate rejects the `<` after the identifier `Omit`, so the
        // generic is preserved. Flipped here because landing Change 2
        // contradicts the old assertion (Requirement 2.3).
        //   EXPECTED (F'): unchanged.
        let src = "type X = Omit<T, id>";

        let tags = scan(src);
        assert!(
            tags.is_empty(),
            "FIXED (Change 2): the generic yields no tag, got: {:?}",
            names(&tags)
        );

        let out = format_tsx(src);
        assert_eq!(out, src, "FIXED (Change 2): the generic is preserved verbatim");
        assert!(out.contains(','), "FIXED (Change 2): the comma separator is preserved");
        assert_eq!(angle_counts(src), angle_counts(&out), "brackets conserved");
    }

    // ----- Root cause 2: member-call generics -----------------------------

    #[test]
    fn member_call_generic_is_misread_and_drops_a_bracket() {
        // FLIPPED EARLY (bugfix task 4.2 / Change 2):
        // `axiosConfig.get<Array<UrlStats>>(url)`. The UNFIXED scanner misread
        // the `.get<` generic (head `Array`) and dropped a `<`. Change 2's gate
        // rejects the `<` after the identifier `get` (value-producer position),
        // so the member-call generic is preserved. Flipped here because landing
        // Change 2 contradicts the old assertion (Requirement 2.1).
        //   EXPECTED (F'): unchanged, both `<` preserved.
        let src = "axiosConfig.get<Array<UrlStats>>(url)";

        let tags = scan(src);
        assert!(
            tags.is_empty(),
            "FIXED (Change 2): the member-call generic yields no tag, got: {:?}",
            names(&tags)
        );

        let out = format_tsx(src);
        assert_eq!(out, src, "FIXED (Change 2): the generic is preserved verbatim");
        assert_eq!(
            angle_counts(src),
            angle_counts(&out),
            "FIXED (Change 2): no `<`/`>` is dropped"
        );
    }

    // ----- Root cause 1: generics inside child-position `{ … }` -----------

    #[test]
    fn generic_component_head_inside_child_container_is_preserved() {
        // SUPERSEDED by the `jsx-in-child-containers-not-wrapped` bugfix
        // (Change A — walk into child-position `{ … }` containers instead of
        // skipping them). The earlier revision of this test pinned Change 1's
        // full-opacity mechanism: the child container was skipped wholesale, so
        // NO `Item` tag was produced. Change A removes that opaque skip and walks
        // the container interior under the SAME expression-position gate, so the
        // `<Item<T> …>` component head IS now recognized as a Start_Tag — but,
        // because its attribute region opens with a `<` (a generic type
        // argument), Change 3 marks it NOT confidently rewritable and the full
        // pass leaves it byte-for-byte unchanged.
        //
        // The generics-spec Property 1 guarantee — the generic construct stays
        // byte-for-byte preserved with every `<`/`>` intact — is UNCHANGED; only
        // the mechanism that preserves it moved from "opaque skip" to
        // "recognize + Change 3 confidence no-op". This test is updated to assert
        // that still-safe behavior.
        //   EXPECTED (F'): the genuine `<div>` is recognized AND the in-container
        //                  `<Item<T> …>` head is recognized as a
        //                  not-confidently-rewritable tag; the full pass preserves
        //                  the generic byte-for-byte with no dropped bracket.
        let src = "return (<div>{list.map(x => <Item<T> key={x} />)}</div>)";

        let tags = scan(src);
        // `<div>` and the in-container `<Item<T> …>` head are both recognized.
        assert_eq!(
            names(&tags),
            vec!["div", "Item"],
            "Change A: the container is walked; both `div` and the `Item` head are recognized"
        );
        // The `Item` head is a not-confidently-rewritable per-tag no-op (Change 3
        // fires inside the container exactly as outside it, Requirement 3.3).
        let item = tags
            .iter()
            .find(|t| t.name == "Item")
            .expect("the `Item` head is recognized");
        assert!(
            !item.confidently_rewritable,
            "Change 3: a `<` in the attribute region poisons the tag's confidence"
        );

        // A full pass leaves the generic inside the container byte-for-byte
        // unchanged: every `<` and `>` is preserved (generics-spec Property 1).
        let out = format_tsx(src);
        assert_eq!(out, src, "the in-container generic head is preserved byte-for-byte");
        assert_eq!(
            angle_counts(src),
            angle_counts(&out),
            "no `<`/`>` is dropped"
        );
    }

    // ----- Root cause 1/3: component-with-generics head -------------------

    #[test]
    fn component_with_generics_head_drops_inner_bracket() {
        // FLIPPED EARLY (bugfix task 5.1 / Change 3, Requirement 2.4). The
        // original exploration assertion captured the UNFIXED behavior: the
        // component head `<EditableList<SearchEngineDomain>>` was delimited as a
        // tag `EditableList` whose attribute region begins with `<`, and it was
        // (wrongly) deemed confidently rewritable, so the inner `<` was dropped
        // on rewrite (`<EditableList SearchEngineDomain>>`). Change 3 treats a
        // `<` encountered in the attribute region as a confidence loss: the tag
        // is marked not-confidently-rewritable (mirroring the `ComplexExpression`
        // poison) and left byte-for-byte unchanged (a safe per-tag no-op,
        // Requirement 4.9). This assertion is therefore flipped here to the
        // now-correct behavior rather than waiting for task 7, because landing
        // Change 3 makes the old (buggy) assertion impossible to keep.
        //   EXPECTED (F'): recognized as a not-confidently-rewritable tag and left
        //                  byte-for-byte unchanged (safe per-tag no-op, Req 2.4).
        let src = "<EditableList<SearchEngineDomain>>";

        let tags = scan(src);
        assert_eq!(names(&tags), vec!["EditableList"]);
        // The span is still recorded so the walk resumes correctly after the
        // tag, but the `<` in the attribute region loses confidence.
        assert_eq!(&src[tags[0].span.clone()], "<EditableList<SearchEngineDomain>");
        assert!(
            !tags[0].confidently_rewritable,
            "FIXED (Change 3): the component-generic head is not confidently rewritable"
        );

        // A full pass leaves the component head byte-for-byte unchanged: every
        // `<` and `>` is preserved (per-tag no-op, Req 4.9).
        let out = format_tsx(src);
        assert_eq!(out, src, "FIXED (Change 3): the component head is preserved verbatim");
        assert_eq!(
            angle_counts(src),
            angle_counts(&out),
            "FIXED (Change 3): no `<`/`>` is dropped"
        );
    }

    // ----- Root cause 2 cascade: mixed statement + JSX --------------------

    #[test]
    fn mixed_statement_and_jsx_cascade_misreads_trailing_generic() {
        // FLIPPED EARLY (bugfix task 4.2 / Change 2, Requirement 2.5). A file
        // interleaving a fragment (`<>`…`</>`), a self-closing tag, and a
        // genuine component, then a trailing TOP-LEVEL generic. The UNFIXED
        // scanner, lacking expression-position context, misread the trailing
        // `useState<Array<Task>>` as a Start_Tag. Change 2's running classifier
        // is drift-proof: it is recomputed from the bytes consumed and never
        // accumulates a nesting count across the fragment / self-closing tag, so
        // the trailing `<` — in `Ident` position after `useState` — is correctly
        // rejected. Flipped here because landing Change 2 contradicts the old
        // assertion; this is exactly the drift/cascade concern (Req 2.5).
        //   EXPECTED (F'): genuine `<br/>`/`<PageLoading .../>` still handled; the
        //                  trailing generic preserved byte-for-byte, no tag.
        let src = "<>\n  <br/>\n  <PageLoading loading={false} />\n</>;\nconst [t] = useState<Array<Task>>([]);";

        let tags = scan(src);
        // Only the genuine tags are recognized; the trailing top-level generic
        // is left alone (no `Array` tag).
        assert_eq!(
            names(&tags),
            vec!["br", "PageLoading"],
            "FIXED (Change 2): the trailing top-level generic is not misread"
        );
        assert!(
            tags.iter().all(|t| t.name != "Array"),
            "FIXED (Change 2): no tag is produced from the trailing generic"
        );

        let out = format_tsx(src);
        // Genuine component tags still wrap/normalize (the self-closing `<br/>`
        // gets a space: `<br />`), and the trailing generic is preserved intact.
        assert!(out.contains("<br />"), "genuine self-closing tag still handled: {out:?}");
        assert!(out.contains("<PageLoading loading={false} />"), "genuine component preserved: {out:?}");
        assert!(
            out.contains("useState<Array<Task>>"),
            "FIXED (Change 2): trailing generic preserved verbatim in {out:?}"
        );
        assert_eq!(
            angle_counts(src),
            angle_counts(&out),
            "FIXED (Change 2): no `<`/`>` is dropped overall"
        );
    }

    // ----- Root cause 3: self-check symmetric-misread blind spot ----------

    #[test]
    fn self_check_fingerprint_blind_spot_is_closed_by_angle_bracket_invariant() {
        // FLIPPED (bugfix task 7 / Change 4, Requirements 2.7, 7.5). This is the
        // motivating self-check regression from task 1, flipped to the corrected
        // behavior and RETAINING the regression-pin that the new invariant
        // closes, re-using the exact same construct:
        //   input : const x = useState<Array<Task>>([]);
        //   output: const x = useState<Array Task>>([]);   (one `<` dropped)
        //
        // ORIGINAL MOTIVATING REGRESSION (Requirement 1.7): a symmetric misread.
        // Both passes misread the generic identically, so the (mis)recognized
        // tag spans coincided and the inter-tag `Text` runs were byte-identical
        // on both sides. The document fingerprints therefore MATCHED and
        // `fingerprints_equivalent` returned `true`, declaring the corruption
        // "structure-preserving" — the self-check blind spot that let a
        // changed-and-invalid output through.
        let input = "const x = useState<Array<Task>>([]);";
        let corrupted_output = "const x = useState<Array Task>>([]);";

        // --- Regression-pin: `fingerprints_equivalent` ALONE is blind ---------
        //
        // The blind spot is a property of the document fingerprint, which omits
        // the `<`/`>` delimiter bytes a tag rewrite is allowed to change. Under a
        // *symmetric* misread the (mis)recognized tag spans coincide on both
        // sides, leaving byte-identical inter-tag `Text` runs AND an identical
        // tag signature — so the two document fingerprints are literally equal
        // and `fingerprints_equivalent` returns `true`.
        //
        // Change 2 now prevents the misread, so this blind-spot condition can no
        // longer be produced through the real scanner (each buffer scans to a
        // single, byte-differing whole-buffer `Text` item). We therefore
        // reconstruct the symmetric-misread fingerprints DIRECTLY, exactly as the
        // unfixed scanner produced them (design Change 4 trace): a shared leading
        // `Text("const x = useState")`, a shared misread `Tag`, and a shared
        // trailing `Text(">([]);")`. Because both sides are identical, the old
        // comparison judges the corruption "equivalent" — the regression this
        // task pins so the hole stays provably closed.
        let misread_tag = TagFingerprint {
            name: "Array".to_string(),
            self_closing: false,
            attributes: vec![(Some("task".to_string()), "Task".to_string())],
            children: Vec::new(),
        };
        let symmetric_items = || {
            vec![
                FingerprintItem::Text("const x = useState".to_string()),
                FingerprintItem::Tag(misread_tag.clone()),
                FingerprintItem::Text(">([]);".to_string()),
            ]
        };
        let input_fp = Fingerprint { items: symmetric_items() };
        let output_fp = Fingerprint { items: symmetric_items() };
        assert!(
            fingerprints_equivalent(&input_fp, &output_fp, false),
            "REGRESSION-PIN: the symmetric-misread fingerprints are equal, so \
             `fingerprints_equivalent` ALONE judges the corruption equivalent — \
             the blind spot the angle-bracket invariant now closes"
        );

        // --- Change 4: the angle-bracket-conservation invariant CATCHES it ----
        //
        // The new invariant is taken over the raw bytes of the WHOLE buffer, not
        // over any tag-excluded fingerprint residue, so the dropped `<` is always
        // observed: `input` has two `<`, `corrupted_output` has one. The engine
        // therefore reverts/errors on this pair and never emits the changed,
        // structurally-invalid output.
        assert_eq!(angle_counts(input).0, 2, "input has both `<` of the generic");
        assert_eq!(angle_counts(corrupted_output).0, 1, "the corruption drops one `<`");
        assert!(
            !angle_brackets_conserved(input, corrupted_output),
            "FIXED (Change 4): the dropped-`<` corruption FAILS the \
             angle-bracket-conservation invariant, so it can never be emitted"
        );

        // --- End-to-end under F': a byte-for-byte no-op -----------------------
        //
        // With Change 2 in place the generic is never misread in the first place,
        // so a real pass leaves the construct byte-for-byte unchanged and no
        // corrupting change is ever produced for the self-check to catch.
        let tags = scan(input);
        assert!(
            tags.is_empty(),
            "FIXED (Change 2): the symmetric-misread generic yields no tag, got: {:?}",
            names(&tags)
        );
        let emitted = format_tsx(input);
        assert_eq!(
            emitted, input,
            "FIXED (Change 2): the generic is preserved, no corrupting change is emitted"
        );
        assert_eq!(
            angle_counts(input),
            angle_counts(&emitted),
            "FIXED: no `<`/`>` is dropped end-to-end"
        );
    }
}

#[cfg(test)]
mod component_generics_head_tests {
    //! Component-with-generics head as a confident no-op (bugfix spec task 5.2,
    //! Change 3, Requirements 2.4, 4.9).
    //!
    //! These are the dedicated unit tests for the confidence loss implemented in
    //! task 5.1: inside [`delimit_start_tag`], a `<` encountered in the attribute
    //! region before the tag terminator (as in `<EditableList<T>>`, where the
    //! character after the element name is `<`) marks the whole start tag
    //! not-confidently-rewritable (`confidently_rewritable == false`), mirroring
    //! the `Complex_Expression_Attribute` poison. The tag's span is still
    //! recorded so the walk resumes correctly after it, but the scanner declines
    //! to parse the TypeScript type-argument list and the engine emits the tag's
    //! original bytes verbatim — a safe per-tag no-op (Requirement 4.9) rather
    //! than dropping the inner `<`.
    //!
    //! Both the plain type parameter `<EditableList<T>>` and the named type
    //! argument `<EditableList<SearchEngineDomain>>` forms are covered. At the
    //! scanner level we assert the recognized tag loses confidence; through a
    //! full `engine::format_once` pass on a `.tsx` path we assert the construct
    //! is left byte-for-byte unchanged with every `<`/`>` preserved.
    //!
    //! This module lives on its own so it never collides with the edits of
    //! `mod tests`, `scanner_integration_tests`, `confidence_tests`,
    //! `confidence_model_tests`, `generics_bug_exploration`, or
    //! `child_container_skip_tests`.

    use super::{scan_start_tags, ScannedTag};
    use crate::config::{Configuration, SchemeId};
    use crate::dialect::{Dialect, ScannerProfile};
    use crate::engine::format_once;
    use std::path::Path;

    /// The JSX/TSX scanner profile under test.
    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// A `.tsx`-oriented configuration: fluid wrapping on, sorting off,
    /// `indent_width = 2`. Fluid wrapping being on is what would make a
    /// confidently-rewritable tag get rewritten — so a byte-for-byte no-op here
    /// proves the component head is genuinely treated as not-confidently-
    /// rewritable rather than merely fitting on one line.
    fn tsx_config() -> Configuration {
        Configuration {
            line_width: 80,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        }
    }

    /// Scan `src` with the JSX profile and `indent_width = 2`, asserting the
    /// scan itself succeeds (the component-generics head is a *confidence loss*,
    /// not a parse failure).
    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("component-generics head scans without a failure")
    }

    /// Run a single `format_once` pass over a `.tsx` input and return the
    /// candidate bytes as a string.
    fn format_tsx(src: &str) -> String {
        let out = format_once(Path::new("file.tsx"), src.as_bytes(), None, &tsx_config())
            .expect("component-generics head formats without an engine error");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    /// Count the `<` and `>` characters over the whole buffer, so a dropped
    /// delimiter is observable even if the surrounding bytes happen to match.
    fn angle_counts(s: &str) -> (usize, usize) {
        (s.matches('<').count(), s.matches('>').count())
    }

    /// The scanner marks a component head whose name is immediately followed by
    /// a generic `<` as not-confidently-rewritable, for both the plain type
    /// parameter and the named type-argument forms (Change 3, Requirement 2.4).
    #[test]
    fn component_generics_head_loses_confidence() {
        for src in ["<EditableList<T>>", "<EditableList<SearchEngineDomain>>"] {
            let tags = scan(src);

            // Exactly one tag is delimited: the component head `EditableList`.
            // The `<` in the attribute region ends the span at the first `>`,
            // so the recorded span is `<EditableList<…>` (up to that first `>`).
            assert_eq!(tags.len(), 1, "exactly one tag is recognized in {src:?}");
            assert_eq!(tags[0].name, "EditableList", "the head name is the element name in {src:?}");

            // The `<` in the attribute region poisons the tag's confidence, so
            // the downstream pipeline must emit it verbatim rather than wrapping
            // or sorting it (per-tag no-op, Requirement 4.9).
            assert!(
                !tags[0].confidently_rewritable,
                "the component-generics head must be not-confidently-rewritable in {src:?}"
            );
        }
    }

    /// A full `format_once` pass leaves the component-with-generics head
    /// byte-for-byte unchanged, with every `<`/`>` preserved — the safe per-tag
    /// no-op the confidence loss guarantees (Requirements 2.4, 4.9).
    #[test]
    fn component_generics_head_is_byte_for_byte_unchanged() {
        for src in ["<EditableList<T>>", "<EditableList<SearchEngineDomain>>"] {
            let out = format_tsx(src);
            assert_eq!(out, src, "the component head is preserved verbatim for {src:?}");
            assert_eq!(
                angle_counts(src),
                angle_counts(&out),
                "no `<`/`>` is dropped for {src:?}"
            );
        }
    }

    /// Regression (bugfix spec task 10.1, Change 3, Requirements 2.4, 4.9): a
    /// component-with-generics head that is FOLLOWED BY REAL ATTRIBUTES and
    /// spans the shape a user actually hit —
    /// `<Select<Array<number>> labelId="task-select-label" multiple value={x}>`
    /// — still delimits to a SINGLE tag whose element name is the component
    /// head (`Select`, never the generic argument `Array`/`number`), loses
    /// confidence because of the `<` in the attribute region (a per-tag no-op,
    /// Requirement 4.9), and records a span that STARTS at the `<Select` open.
    ///
    /// The attribute-less `<EditableList<SearchEngineDomain>>` case above only
    /// exercises the head-immediately-followed-by-`>` shape; this pins the
    /// "generics head PLUS trailing attributes" shape, where the nested generic
    /// `<Array<number>>` and the real attributes must not derail the scan.
    #[test]
    fn component_generics_head_with_trailing_attributes_loses_confidence() {
        // Nested-generic head followed by several real attributes.
        let nested = "<Select<Array<number>> labelId=\"task-select-label\" multiple value={x}>";
        // Single-generic head followed by plain boolean attributes.
        let single = "<Comp<T> a b c>";

        for (src, expected_name) in [(nested, "Select"), (single, "Comp")] {
            let tags = scan(src);

            // Exactly one tag: the component head. The nested generic
            // (`Array`/`number`) and the trailing attributes must not spawn
            // extra tags.
            assert_eq!(tags.len(), 1, "exactly one tag is recognized in {src:?}");

            // The element name is the component head, never the generic
            // argument identifier.
            assert_eq!(
                tags[0].name, expected_name,
                "the element name is the component head in {src:?}"
            );

            // The `<` in the attribute region poisons the whole tag's
            // confidence (per-tag no-op, Requirement 4.9).
            assert!(
                !tags[0].confidently_rewritable,
                "the generics-head-with-attributes tag must be not-confidently-rewritable in {src:?}"
            );

            // The recorded span starts at the `<` that opens the component head.
            let span_src = &src[tags[0].span.clone()];
            let open = format!("<{expected_name}");
            assert!(
                span_src.starts_with(&open),
                "the span must start at the {open:?} open in {src:?}, got {span_src:?}"
            );
            assert_eq!(
                tags[0].span.start,
                0,
                "the span starts at the offset of the `<` opening the head in {src:?}"
            );
        }
    }
}

#[cfg(test)]
mod child_container_skip_tests {
    //! Child-position `{ … }` opaque skip (bugfix spec task 3.3, Change 1).
    //!
    //! These tests pin the behavior of the child-container skip branch added to
    //! [`scan_start_tags`] in task 3.2: for the JSX/TSX profile, a top-level
    //! child-position `{ … }` is delimited with the confidence-aware JS lexer
    //! ([`delimit_braced`]) and skipped whole, so its contents are never scanned
    //! for start tags. This directly prevents the scanner from walking into
    //! JavaScript/TypeScript expression text — generics and `<` operators — and
    //! misreading it as a tag (Requirements 2.2, 1.2). An unterminated `{` at
    //! end of input is the hard-parse-failure case (Requirement 7.1).
    //!
    //! IMPORTANT: the child-container branch currently fires on ANY top-level
    //! `{` for the JSX/TSX profile (the `last_significant` context gate is added
    //! later, in task 4). So these inputs are deliberately *bare* child-position
    //! containers — a leading `{ … }` at top level — which is exactly what this
    //! branch handles today. They are NOT wrapped in statement-blocks that would
    //! depend on the task-4 gate.
    //!
    //! Lives in its own module so it never collides with the edits of
    //! `mod tests`, `scanner_integration_tests`, `confidence_tests`,
    //! `confidence_model_tests`, or `generics_bug_exploration`.

    use super::{scan_start_tags, ParseFailure, ScannedTag};
    use crate::dialect::{Dialect, ScannerProfile};

    /// The JSX/TSX scanner profile under test (the only profile that enables
    /// `scan_child_expression_containers`).
    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan `src` with the JSX profile and `indent_width = 2`, asserting the
    /// scan itself succeeds.
    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("scan should succeed")
    }

    /// Scan `src` with the JSX profile and `indent_width = 2`, asserting the
    /// scan fails, and return the [`ParseFailure`].
    fn scan_err(src: &str) -> ParseFailure {
        scan_start_tags(src, &jsx(), 2)
            .expect_err("scan should fail with a ParseFailure")
    }

    #[test]
    fn generic_inside_container_produces_no_tag() {
        // `{ generic<here> }` — the `<here>` inside the container must NOT be
        // scanned as a tag: the whole container is skipped as opaque.
        let tags = scan("{ generic<here> }");
        assert!(
            tags.is_empty(),
            "no tag may be produced from the container contents, got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn jsx_comment_container_produces_no_tag() {
        // `{/* comment with <Tag> */}` — a JSX comment is a child-position
        // container whose block-comment state is absorbed by `delimit_braced`;
        // the `<Tag>` inside the comment must never be scanned as a tag.
        let tags = scan("{/* comment with <Tag> */}");
        assert!(
            tags.is_empty(),
            "no tag may be produced from a JSX comment container, got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn string_guarded_closing_brace_does_not_end_skip_early() {
        // `{ s = "}" }` — the `}` inside the string literal must NOT prematurely
        // close the container. The confidence-aware lexer treats it as string
        // content, so the real closer is the final `}`, and the whole container
        // is skipped. (No `<` here, but this pins that the skip spans the string
        // correctly rather than ending at the embedded `}`.)
        let src = "{ s = \"}\" }";
        let tags = scan(src);
        assert!(
            tags.is_empty(),
            "the string-guarded `}}` must not end the skip early; got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn nested_braces_container_produces_no_tag() {
        // `{ { } }` — nested braces must balance within the single skipped
        // container; the walk must not treat the inner `}` as the container's
        // closer and resume scanning prematurely.
        let tags = scan("{ { } }");
        assert!(
            tags.is_empty(),
            "nested braces must be skipped as one container; got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn regex_with_embedded_brace_does_not_end_skip_early() {
        // `{ re.test(/a}b/) }` — the `}` inside the regex literal must NOT
        // prematurely end the skip. Whether `delimit_braced` keeps full regex
        // confidence (treating `/a}b/` as a regex so the `}` is literal) or
        // falls back best-effort, the container is skipped and NO tag is
        // produced from its contents. This test asserts the observable outcome
        // (no tag) that must hold on either confidence path.
        let tags = scan("{ re.test(/a}b/) }");
        assert!(
            tags.is_empty(),
            "the regex-embedded `}}` must not end the skip early and no tag may \
             be produced; got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unterminated_brace_at_eof_is_a_parse_failure() {
        // An unterminated `{` at end of input is a hard parse failure
        // (Requirement 7.1): the braced expression ran off the end of input.
        let failure = scan_err("{ generic<here>");
        assert_eq!(
            failure.message, "unterminated braced expression at end of input",
            "unterminated `{{` at EOF must report the braced-expression failure"
        );
        // The reported location points at the opening `{` (1-based line/column).
        assert_eq!(failure.line, 1);
        assert_eq!(failure.column, 1);
    }
}

#[cfg(test)]
mod start_tag_position_tests {
    //! Decision-table unit tests for the drift-proof expression-position gate
    //! (task 4.1, Change 2). These exercise [`is_start_tag_position`] directly
    //! against every [`LastToken`] classification crossed with the two
    //! name-start outcomes, matching the design's decision table:
    //!
    //! | `last_significant` before `<` | `<` interpreted as | accept? |
    //! |---|---|---|
    //! | `None` / `OperatorOrOpen` / `TagClose` | JSX element `Start_Tag` | yes |
    //! | `Ident` (identifier, `)`, `]`, literal) | `TypeScript` `<` (generic / `<`) | no |
    //!
    //! Wiring `last_significant` into the walk is task 4.2; here the classifier
    //! value is supplied directly so the predicate's rule is pinned in isolation
    //! (Requirements 2.1, 2.3, 2.5, 3.3).

    use super::*;
    use crate::dialect::Dialect;

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Byte offset of the `<` in `src`; the predicate expects the offset of the
    /// `<` itself and looks at `lt_offset + 1` for the name start.
    fn lt_at(src: &str) -> usize {
        src.find('<').expect("test source must contain a `<`")
    }

    #[test]
    fn accepts_name_start_in_expression_position() {
        // `None`, `OperatorOrOpen`, and `TagClose` are all expression position:
        // a `<` whose next char is a name start begins a JSX element.
        let profile = jsx();
        let src = "<div>";
        let lt = lt_at(src);
        for last in [
            LastToken::None,
            LastToken::OperatorOrOpen,
            LastToken::TagClose,
        ] {
            assert!(
                is_start_tag_position(src, lt, last, &profile),
                "expected accept for {last:?} before `<div>`"
            );
        }
    }

    #[test]
    fn rejects_name_start_after_value_producer() {
        // `Ident` (identifier, `)`, `]`, literal) means the `<` is a TypeScript
        // generic / less-than, never a JSX element — reject even though the next
        // char is a valid name start (`Array<Task>`, `Record<...>`, `useState<`).
        let profile = jsx();
        for src in ["<Task>", "<Array>", "<string>"] {
            let lt = lt_at(src);
            assert!(
                !is_start_tag_position(src, lt, LastToken::Ident, &profile),
                "expected reject for Ident position before {src:?}"
            );
        }
    }

    #[test]
    fn rejects_when_next_char_is_not_a_name_start_regardless_of_last_token() {
        // Rule 1 of the predicate (name-start) must hold independently of the
        // position rule: a `<` followed by a non-name-start char is never a tag,
        // for ANY `LastToken` classification.
        let profile = jsx();
        // `< ` (space), `</` (slash — not a name start), `<=` (operator), and
        // `<1` (digit is not a tag-name start) all fail the name-start rule.
        for src in ["< x", "</div>", "<= y", "<1>"] {
            let lt = lt_at(src);
            for last in [
                LastToken::None,
                LastToken::Ident,
                LastToken::OperatorOrOpen,
                LastToken::TagClose,
            ] {
                assert!(
                    !is_start_tag_position(src, lt, last, &profile),
                    "expected reject for {src:?} with {last:?} (not a name start)"
                );
            }
        }
    }

    #[test]
    fn decision_table_is_exhaustive_over_last_token() {
        // Cross every `LastToken` with a name-start `<`: accept iff not `Ident`.
        let profile = jsx();
        let src = "<Component>";
        let lt = lt_at(src);
        let cases = [
            (LastToken::None, true),
            (LastToken::OperatorOrOpen, true),
            (LastToken::TagClose, true),
            (LastToken::Ident, false),
        ];
        for (last, expected) in cases {
            assert_eq!(
                is_start_tag_position(src, lt, last, &profile),
                expected,
                "decision-table mismatch for {last:?}"
            );
        }
    }

    #[test]
    fn end_of_input_after_lt_is_never_a_tag() {
        // A trailing `<` with nothing after it fails the name-start rule and is
        // rejected for every classification — the predicate stays panic-free at
        // the end-of-input boundary.
        let profile = jsx();
        let src = "<";
        let lt = lt_at(src);
        for last in [
            LastToken::None,
            LastToken::Ident,
            LastToken::OperatorOrOpen,
            LastToken::TagClose,
        ] {
            assert!(
                !is_start_tag_position(src, lt, last, &profile),
                "a dangling `<` at EOF must never be a tag ({last:?})"
            );
        }
    }
}

#[cfg(test)]
mod gate_integration_tests {
    //! End-to-end integration coverage for the drift-proof expression-position
    //! gate wired into [`scan_start_tags`] (bugfix spec task 4.3, Change 2).
    //!
    //! The pure decision-table matrix is pinned directly against
    //! [`is_start_tag_position`] by `start_tag_position_tests` (task 4.1). This
    //! module complements that by driving the WHOLE scan walk, so it exercises
    //! the running `last_significant` maintenance of task 4.2: which element
    //! names a real input produces, which generic heads produce none, and the
    //! precedence of the tag-closing `>` (`TagClose`) over a bare `>` operator
    //! (`OperatorOrOpen`).
    //!
    //! Design facts honored here (see Change 2 and the task-4.2 code):
    //! - The `<`-gate applies only for JSX/TSX.
    //! - The child-container `{` skip fires only when `last_significant` is
    //!   `None` or `TagClose`.
    //! - JSX inter-element TEXT sets `Ident`, so a genuine child element that
    //!   immediately follows TEXT is a SAFE no-op (preserved, not recognized).
    //!   Accept cases therefore use adjacency (`<ul><li>`) or an expression
    //!   introducer, never `text<child>`.
    //!
    //! Requirements: 2.1, 2.3, 2.5, 3.3.

    use super::{scan_start_tags, LastToken, ScannedTag};
    use crate::dialect::{Dialect, ScannerProfile};

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan `src` under the JSX profile (indent width 2), asserting success.
    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("scan should succeed")
    }

    /// The ordered element names located in `src`.
    fn names(src: &str) -> Vec<String> {
        scan(src).iter().map(|t| t.name.to_string()).collect()
    }

    /// Assert `src` produces NO start tags (a generic / less-than head that the
    /// gate correctly declines). Uses the message form so the failure prints the
    /// offending names and satisfies `clippy::assert_is_empty`.
    fn assert_no_tags(src: &str) {
        let got = names(src);
        assert!(got.is_empty(), "expected no tags for {src:?}, got {got:?}");
    }

    // ----- accept: expression-introduced JSX ------------------------------

    #[test]
    fn return_introduces_a_recognized_element() {
        // `return` is an expression introducer (`OperatorOrOpen`), so the `<`
        // after it begins a JSX element.
        assert_eq!(names("const f = () => { return <div a b c>x</div>; };"), vec!["div"]);
    }

    #[test]
    fn arrow_introduces_a_recognized_element() {
        // The arrow `=>` introduces an expression position.
        assert_eq!(names("const f = x => <Item/>;"), vec!["Item"]);
    }

    #[test]
    fn yield_and_await_introduce_recognized_elements() {
        assert_eq!(names("function* g() { yield <x/>; }"), vec!["x"]);
        assert_eq!(names("async function h() { await <y/>; }"), vec!["y"]);
    }

    #[test]
    fn child_container_sibling_element_is_recognized() {
        // After a tag-close `>` (`TagClose`) a `{ … }` child container is
        // skipped and the following sibling `<span>` is still recognized.
        assert_eq!(names("<div>{value}<span/></div>"), vec!["div", "span"]);
    }

    // ----- reject: TypeScript generics / less-than ------------------------

    #[test]
    fn statement_position_generic_produces_no_tag() {
        // `useState<Array<Task>>` sits after an `=` then identifiers; each `<`
        // is in `Ident` position, so NO tag is produced and the bytes are
        // preserved verbatim by the caller.
        assert_no_tags("const [t] = useState<Array<Task>>([]);");
    }

    #[test]
    fn multi_arg_generics_produce_no_tag() {
        assert_no_tags("type A = Record<subjectKey, string>;");
        assert_no_tags("type B = Omit<T, id>;");
    }

    #[test]
    fn member_call_generic_produces_no_tag() {
        assert_no_tags("axiosConfig.get<Array<UrlStats>>(url);");
    }

    #[test]
    fn less_than_comparison_produces_no_tag() {
        // `a < b` — the `<` follows an identifier (`Ident`) and is a comparison
        // operator, never a tag.
        assert_no_tags("const r = a < b;");
    }

    // ----- `>` precedence: TagClose vs OperatorOrOpen ---------------------

    #[test]
    fn tag_closing_gt_sets_tagclose_so_adjacent_child_is_accepted() {
        // The `>` that closes `<ul>` is a tag terminator, classified `TagClose`
        // (NOT the generic `OperatorOrOpen` of a bare `>`), so the adjacent
        // child `<li>` — whose `<` sits immediately after that `>` — is accepted
        // as an element. Both `ul` and `li` are recognized.
        assert_eq!(names("<ul><li>a</li></ul>"), vec!["ul", "li"]);

        // Direct predicate confirmation of the precedence: with the classifier
        // the scanner assigns after a tag-closing `>` (`TagClose`), the `<li>`
        // position is accepted.
        let src = "<li>";
        let profile = jsx();
        let lt = src.find('<').unwrap();
        assert!(super::is_start_tag_position(src, lt, LastToken::TagClose, &profile));
    }

    #[test]
    fn bare_gt_operator_is_operatoropen_and_harmless() {
        // A bare `>` / `>>` used as a comparison / shift operator classifies as
        // `OperatorOrOpen`. In the generic-drop scenarios the char following the
        // operator is whitespace or an identifier (not a tag-name start after a
        // `<`), so the gate is harmless and no tag is produced.
        assert_no_tags("const r = a > b;");
        assert_no_tags("const s = x >> y;");

        // The `>>` tail of a nested generic (`useState<Array<Task>>`) likewise
        // produces no tag: the whole construct is preserved.
        assert_no_tags("const [t] = useState<Array<Task>>([]);");

        // Direct predicate confirmation: in `OperatorOrOpen` position a `<`
        // FOLLOWED BY A NAME START *would* be a tag (that is the expression
        // position rule), but a bare `>` operator is only `OperatorOrOpen` when
        // its following context is not such a `<name`; the harmless outcome
        // above reflects that the generic tails are never `<name` sequences.
        let profile = jsx();
        let src = "<Component>";
        let lt = src.find('<').unwrap();
        // Sanity: OperatorOrOpen DOES accept a genuine `<name` (expression
        // position), which is why precedence matters — the tag-closing `>` must
        // be TagClose, not conflated with a bare operator.
        assert!(super::is_start_tag_position(src, lt, LastToken::OperatorOrOpen, &profile));
    }
}

#[cfg(test)]
mod child_container_jsx_exploration {
    //! Bugfix exploration tests, FLIPPED to the corrected behavior
    //! (jsx-in-child-containers-not-wrapped spec tasks 1 and 4). These reuse the
    //! SAME constructs task 1 captured as no-op counterexamples; each assertion
    //! is flipped from "the UNFIXED scanner leaves the in-container tag
    //! invisible/unwrapped" to "F' recognizes and fluid-wraps the in-container
    //! tag identically to the same tag outside a container".
    //!
    //! ROOT CAUSE (now fixed by Change A): the former full-opacity branch in
    //! [`scan_start_tags`] delimited every child-position `{ … }` and jumped past
    //! it WITHOUT scanning the interior, hiding the inner JSX. Change A walks the
    //! container interior under the same expression-position gate, so a `.map()`
    //! callback, ternary, `&&`/`||` conditional, or nested container now yields
    //! its genuine JSX `Start_Tag`, while generics inside the same containers are
    //! still rejected.
    //!
    //! Lives in its own module so it never collides with the edits of
    //! `mod tests`, `scanner_integration_tests`, `confidence_tests`,
    //! `confidence_model_tests`, `generics_bug_exploration`,
    //! `component_generics_head_tests`, `child_container_skip_tests`, or
    //! `child_container_walk_tests`.

    use super::{scan_start_tags, ScannedTag};
    use crate::dialect::{Dialect, ScannerProfile};

    /// The JSX/TSX scanner profile under test.
    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    /// Scan `src` with the JSX profile and `indent_width = 2`.
    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("scan should succeed")
    }

    /// The element names produced by the scanner, in order.
    fn names(src: &str) -> Vec<&str> {
        scan(src).into_iter().map(|t| t.name).collect()
    }

    /// Run `format_once` over a `.tsx` input at the given narrow `line_width`
    /// and return the output bytes, asserting the pass succeeds.
    #[cfg(feature = "wasm")]
    fn format_tsx(src: &str, line_width: u32) -> Vec<u8> {
        use crate::config::{Configuration, SchemeId};
        let cfg = Configuration {
            line_width,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };
        crate::engine::format_document(std::path::Path::new("c.tsx"), src.as_bytes(), None, &cfg)
            .expect("format_document should succeed")
            .unwrap_or_else(|| src.as_bytes().to_vec())
    }

    #[test]
    fn map_callback_jsx_is_recognized_and_wrapped() {
        // (a) Requirement 2.1 — a `.map()` callback inside a child container.
        // FLIPPED: `<Button>` IS recognized and the overflowing tag IS wrapped.
        let src = "<ul>{pages.map(p => <Button variant=\"ghost\" size=\"sm\" onClick={go} disabled={busy} aria-label=\"next\" className=\"x\">)}</ul>";
        assert!(
            names(src).contains(&"Button"),
            "FIXED: the in-container <Button> is recognized, got: {:?}",
            names(src)
        );
        #[cfg(feature = "wasm")]
        assert_in_container_wraps_like_out_of_container(
            "pages.map(p => (\n{TAG}\n))",
            "<Button variant=\"ghost\" size=\"sm\" onClick={go} disabled={busy} aria-label=\"next\" className=\"x\">",
        );
    }

    #[test]
    fn ternary_jsx_is_recognized_and_wrapped() {
        // (b) Requirement 2.2 — a ternary `? :` inside a child container.
        // FLIPPED: `<IconButton>` IS recognized and wrapped.
        let src = "<nav>{open ? (<IconButton color=\"primary\" size=\"large\" edge=\"end\" onClick={toggle} aria-label=\"menu\" className=\"btn\">) : <HeaderLanguage/>}</nav>";
        assert!(
            names(src).contains(&"IconButton"),
            "FIXED: the in-container <IconButton> is recognized, got: {:?}",
            names(src)
        );
        #[cfg(feature = "wasm")]
        assert_in_container_wraps_like_out_of_container(
            "open ? (\n{TAG}\n) : null",
            "<IconButton color=\"primary\" size=\"large\" edge=\"end\" onClick={toggle} aria-label=\"menu\" className=\"btn\">",
        );
    }

    #[test]
    fn and_conditional_jsx_is_recognized_and_wrapped() {
        // (c) Requirement 2.3 — an `&&` conditional inside a child container.
        // FLIPPED: `<Panel>` IS recognized and wrapped.
        let src = "<div>{loggedIn && (<Panel title=\"Account\" variant=\"outlined\" elevation={2} square dense className=\"p\">)}</div>";
        assert!(
            names(src).contains(&"Panel"),
            "FIXED: the in-container <Panel> is recognized, got: {:?}",
            names(src)
        );
        #[cfg(feature = "wasm")]
        assert_in_container_wraps_like_out_of_container(
            "loggedIn && (\n{TAG}\n)",
            "<Panel title=\"Account\" variant=\"outlined\" elevation={2} square dense className=\"p\">",
        );
    }

    #[test]
    fn nested_container_jsx_is_recognized_and_wrapped() {
        // (d) Requirement 2.4 — a nested child container. FLIPPED: BOTH <Row>
        // and the nested-container <Cell> are recognized, and the overflowing
        // nested <Cell> wraps onto continuation lines.
        let compact = "<table>{rows.map(r => <Row key={r.id}>{r.cells.map(c => <Cell align=\"center\" padding=\"none\" className=\"c\" layout=\"fixed\" />)}</Row>)}</table>";
        let got = names(compact);
        assert!(got.contains(&"Row"), "FIXED: <Row> is recognized, got: {got:?}");
        assert!(got.contains(&"Cell"), "FIXED: the nested-container <Cell> is recognized, got: {got:?}");
        #[cfg(feature = "wasm")]
        {
            // A realistic multi-line layout: the nested <Cell> sits on its own
            // line at a modest indent, so fluid wrapping clearly applies.
            let src = "\
<table>
  {rows.map(r => (
    <Row key={r.id}>
      {r.cells.map(c => (
        <Cell align=\"center\" padding=\"none\" className=\"c\" layout=\"fixed\" scope=\"col\" />
      ))}
    </Row>
  ))}
</table>
";
            let out = String::from_utf8(format_tsx(src, 40)).expect("utf8");
            assert_ne!(out, src, "the overflowing nested <Cell> wraps onto continuation lines");
            // Attribute values preserved.
            assert!(out.contains("align=\"center\""));
            assert!(out.contains("padding=\"none\""));
            assert!(out.contains("layout=\"fixed\""));
            // Structure is preserved: every `<` and `>` survives.
            assert_eq!(
                src.bytes().filter(|&b| b == b'<' || b == b'>').count(),
                out.bytes().filter(|&b| b == b'<' || b == b'>').count(),
                "no angle bracket added or dropped"
            );
            // Idempotent.
            assert_eq!(format_tsx(&out, 40), out.as_bytes(), "repacked nested output is a fixed point");
        }
    }

    #[test]
    #[cfg(feature = "wasm")]
    fn real_world_header_snippet_is_repacked() {
        // (e) Requirements 2.5, 2.6, 3.10 — a real-world header fragment combining
        // a `.map()`, a ternary, an `&&` conditional, and nested containers
        // rendering an overflowing <IconButton> one-attribute-per-line inside a
        // `{ … }` container. FLIPPED: the one-per-line <IconButton> IS repacked
        // into fluid form, and the whole pass is idempotent and structure-safe.
        let src = "\
<header>
  {items.map(item => (
    <IconButton
      color=\"primary\"
      size=\"large\"
      edge=\"end\"
      onClick={item.onClick}
      aria-label={item.label}
      className=\"header-icon-button\"
    >
      {item.icon}
    </IconButton>
  ))}
  {open ? (<span a=\"1\" b=\"2\" c=\"3\" d=\"4\" e=\"5\" f=\"6\" g=\"7\" h=\"8\">x</span>) : null}
  {loggedIn && <Avatar userName=\"alice\" sizePx={32} rounded showStatus statusColor=\"green\" />}
</header>
";
        let out = format_tsx(src, 40);
        assert_ne!(
            out,
            src.as_bytes(),
            "FIXED: the one-attribute-per-line <IconButton> inside the container is repacked"
        );
        let out_str = String::from_utf8(out.clone()).expect("utf8");
        // The one-per-line IconButton attributes are repacked: at least two
        // attributes now share a continuation line.
        assert!(
            out_str.contains("color=\"primary\" size=\"large\"")
                || out_str.contains("size=\"large\" edge=\"end\""),
            "repacked IconButton must put multiple attributes on a line, got:\n{out_str}"
        );
        // Structure preserved: angle-bracket count unchanged.
        assert_eq!(
            src.bytes().filter(|&b| b == b'<' || b == b'>').count(),
            out_str.bytes().filter(|&b| b == b'<' || b == b'>').count(),
            "no angle bracket added or dropped"
        );
        // Idempotent: a second pass over the output is a no-op.
        let again = format_tsx(&out_str, 40);
        assert_eq!(again, out, "the repacked header is a fixed point");
    }

    #[test]
    fn generics_inside_containers_still_produce_no_tag() {
        // (3.1, flip-safe guard) The generics/`<`-operator constructs inside the
        // SAME container shapes must NOT become tags — the fix recognizes genuine
        // JSX without turning generics into tags.
        for src in [
            "<div>{items.map(x => someGeneric<T>())}</div>",
            "<div>{cfg.get<Array<Url>>()}</div>",
            "<div>{x < y && z}</div>",
            "<div>{open ? useState<Array<Task>>() : null}</div>",
        ] {
            let got = names(src);
            // Only the surrounding `<div>` (and any genuine JSX) is recognized;
            // no generic identifier becomes a tag.
            assert!(
                got.iter().all(|&n| n == "div"),
                "no generic may become a tag in `{src}`, got: {got:?}"
            );
            #[cfg(feature = "wasm")]
            assert_eq!(
                format_tsx(src, 40),
                src.as_bytes(),
                "FIXED: the in-container generic is preserved byte-for-byte in `{src}`"
            );
        }
    }

    #[test]
    #[cfg(feature = "wasm")]
    fn same_tag_in_and_out_of_container_formats_identically() {
        // (2.6) Consistency: the same genuine JSX Start_Tag at column 0 wraps to
        // the identical attribute line grouping whether it sits inside a child
        // container or at a top-level child position.
        assert_in_container_wraps_like_out_of_container(
            "cond && (\n{TAG}\n)",
            "<Banner title=\"welcome\" subtitle=\"hello there\" variant=\"info\" dismissible closable=\"yes\">",
        );
    }

    /// Assert that a genuine JSX `Start_Tag`, when embedded INSIDE a child
    /// container `template` (with `{TAG}` marking where the tag goes), wraps to
    /// the same per-attribute line grouping as the identical tag placed at a
    /// top-level child position — modulo the container-relative base indent.
    ///
    /// The comparison is by the SEQUENCE of attribute tokens on each
    /// continuation line (whitespace-insensitive within a line), so a deeper
    /// base indent inside the container does not cause a spurious mismatch.
    #[cfg(feature = "wasm")]
    fn assert_in_container_wraps_like_out_of_container(template: &str, tag: &str) {
        // Both forms place the tag's `<` at column 0 so the comparison isolates
        // the wrapping decision from the base-indent column (the "modulo
        // container-relative base indent" caveat of Requirement 2.6): the
        // container template opens `(\n{TAG}` (tag at col 0) and the
        // out-of-container form likewise starts the tag at col 0.
        let in_container = format!("<root>\n{{{}}}\n</root>", template.replace("{TAG}", tag));
        let out_of_container = format!("<root>\n{tag}\n</root>");

        let wrapped_in = String::from_utf8(format_tsx(&in_container, 40)).expect("utf8");
        let wrapped_out = String::from_utf8(format_tsx(&out_of_container, 40)).expect("utf8");

        let groups_in = attribute_line_groups(&wrapped_in, tag);
        let groups_out = attribute_line_groups(&wrapped_out, tag);
        assert_eq!(
            groups_in, groups_out,
            "in-container wrapping must match out-of-container wrapping\nin:\n{wrapped_in}\nout:\n{wrapped_out}"
        );
        // Both must actually have wrapped (more than one group) for the match to
        // be meaningful at this width.
        assert!(
            groups_out.len() > 1,
            "test premise: the tag must overflow and wrap at width 40\nout:\n{wrapped_out}"
        );
    }

    /// Extract, from a formatted output containing the start tag whose element
    /// name matches `tag`'s name, the per-line grouping of attribute tokens: a
    /// vector of lines, each a vector of whitespace-split tokens, starting at the
    /// element name and ending at the tag terminator. Used to compare wrapping
    /// shape independent of base indentation.
    #[cfg(feature = "wasm")]
    fn attribute_line_groups(output: &str, tag: &str) -> Vec<Vec<String>> {
        // Element name is the identifier right after the leading `<` of `tag`.
        let name: String = tag[1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        let open = format!("<{name}");
        let start = output.find(&open).expect("output must contain the start tag");
        // The tag ends at the first `>` that terminates it. Because wrapping only
        // reflows whitespace, scanning to the next `>` at column start or any `>`
        // after the last attribute is sufficient for these fixtures.
        let rest = &output[start..];
        let end_rel = rest.find('>').expect("tag terminator") + 1;
        let tag_text = &rest[..end_rel];
        tag_text
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|toks| !toks.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod child_container_preservation_baseline {
    //! Preservation baseline (jsx-in-child-containers-not-wrapped spec task 2):
    //! records the UNFIXED behavior `F` for NON-buggy (`¬C`) inputs so the
    //! task-5 corpora can prove `F'(X) = F(X)`.
    //!
    //! Observation-first methodology: each assertion here captures what the
    //! UNFIXED scanner/engine already does for a `¬C` input (generics inside and
    //! outside containers, component-with-generics heads, expression-introduced
    //! JSX, out-of-container JSX, other dialects, idempotence), so the fix is
    //! held to preserving exactly that behavior.
    //!
    //! All tests here PASS on unfixed code and must CONTINUE to pass after the
    //! fix (preservation).
    //!
    //! Lives in its own module so it never collides with the other scanner test
    //! modules.

    use super::{scan_start_tags, ScannedTag};
    use crate::dialect::{Dialect, ScannerProfile};

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    fn scan_with<'a>(src: &'a str, profile: &ScannerProfile) -> Vec<ScannedTag<'a>> {
        scan_start_tags(src, profile, 2).expect("scan should succeed")
    }

    fn names_with(src: &str, profile: &ScannerProfile) -> Vec<String> {
        scan_with(src, profile)
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    fn jsx_names(src: &str) -> Vec<String> {
        names_with(src, &jsx())
    }

    // ----- generics: NO tag, both at statement position AND in a container ---

    #[test]
    fn generics_produce_no_tag_in_and_out_of_containers() {
        // (3.1, 3.2) Each generic / comparison yields NO tag at statement
        // position AND wrapped as a child-container's contents. `last_significant
        // == Ident` at every inner `<`.
        let forms = [
            "const [t] = useState<Array<Task>>([]);",
            "{useState<Array<Task>>([])}",
            "type R = Record<K, V>;",
            "{Record<K, V>}",
            "type O = Omit<T, id>;",
            "{Omit<T, id>}",
            "const r = axios.get<Array<X>>(url);",
            "{axios.get<Array<X>>(url)}",
            "const b = a < b;",
            "{a < b}",
        ];
        for src in forms {
            assert!(
                jsx_names(src).is_empty(),
                "UNFIXED baseline: `{src}` must produce no tag, got: {:?}",
                jsx_names(src)
            );
        }
    }

    // ----- component-with-generics head: a not-confidently-rewritable tag ----

    #[test]
    fn component_with_generics_head_out_of_container_is_unconfident_tag() {
        // (3.3) `<EditableList<SearchEngineDomain>>` OUTSIDE a container scans to
        // a single tag that is NOT confidently rewritable (Change 3). This is a
        // `¬C` input: the out-of-container head is already recognized by `F`, so
        // the fix must preserve it. (The IN-container head is a `C` input — the
        // UNFIXED scanner skips the whole container and produces no tag — so it
        // is asserted in task 4's flip, not here.)
        let src = "<EditableList<SearchEngineDomain> a=\"1\">";
        let tags = scan_with(src, &jsx());
        assert_eq!(
            tags.len(),
            1,
            "UNFIXED baseline: `{src}` must scan to exactly one tag, got: {:?}",
            tags.iter().map(|t| t.name).collect::<Vec<_>>()
        );
        assert!(
            !tags[0].confidently_rewritable,
            "UNFIXED baseline: `{src}` head must be not-confidently-rewritable"
        );
    }

    // ----- expression-introduced JSX: recognized (NOT a child container) -----

    #[test]
    fn expression_introduced_jsx_is_recognized() {
        // (3.4) These `{` are arrow-/function-body or non-child positions, not
        // JSX child containers, so the JSX after the introducer is recognized.
        assert_eq!(jsx_names("return <div a b c>;"), vec!["div"]);
        assert_eq!(jsx_names("const f = x => <Item/>;"), vec!["Item"]);
        assert_eq!(jsx_names("function* g() { yield <x/>; }"), vec!["x"]);
        assert_eq!(jsx_names("async () => { await <y/>; }"), vec!["y"]);
        assert_eq!(jsx_names("const f = () => { return <div/>; };"), vec!["div"]);
    }

    // ----- out-of-container JSX: recognized -------------------------------

    #[test]
    fn out_of_container_jsx_is_recognized() {
        // (3.5) Top-level children, nested `<ul><li>`, self-closing.
        assert_eq!(jsx_names("<ul><li>a</li></ul>"), vec!["ul", "li"]);
        assert_eq!(jsx_names("<br/>"), vec!["br"]);
        assert_eq!(
            jsx_names("<div><span>a</span><span>b</span></div>"),
            vec!["div", "span", "span"]
        );
    }

    // ----- other dialects: unchanged scan path ---------------------------

    #[test]
    fn other_dialects_scan_unchanged() {
        // (3.6) HTML/Vue/Astro/Svelte take the non-JSX path; representative tags
        // are recognized as today.
        let html = Dialect::Html.scanner_profile();
        assert_eq!(names_with("<div class=\"a\" id=\"b\">", &html), vec!["div"]);
        let vue = Dialect::Vue.scanner_profile();
        assert_eq!(names_with("<Comp :class=\"x\" v-if=\"y\">", &vue), vec!["Comp"]);
    }

    // ----- idempotence baseline for genuine out-of-container tags ---------

    #[test]
    fn genuine_tags_format_once_is_a_fixed_point_baseline() {
        // (3.7) `format_once(format_once(x)) == format_once(x)` for genuine
        // out-of-container tags — the idempotence property the fix must keep.
        use crate::config::{Configuration, SchemeId};
        let cfg = Configuration {
            line_width: 24,
            fluid_wrapping: true,
            attribute_sorting: false,
            attribute_order_scheme: SchemeId::Standard,
            indent_width: 2,
            use_tabs: false,
            vue: true,
            astro: true,
            svelte: true,
        };
        let inputs = [
            "<div class=\"container\" id=\"main\" data-role=\"banner\">x</div>",
            "<ul><li>a</li><li>b</li></ul>",
        ];
        for src in inputs {
            let once = crate::engine::format_once(
                std::path::Path::new("c.tsx"),
                src.as_bytes(),
                None,
                &cfg,
            )
            .expect("first pass succeeds");
            let twice =
                crate::engine::format_once(std::path::Path::new("c.tsx"), &once, None, &cfg)
                    .expect("second pass succeeds");
            assert_eq!(
                twice, once,
                "UNFIXED baseline: format_once must be a fixed point for `{src}`"
            );
        }
    }
}

#[cfg(test)]
mod child_container_walk_tests {
    //! Unit tests for Change A — walking INTO child-position `{ … }` JSX
    //! expression containers (jsx-in-child-containers-not-wrapped spec task 3.2).
    //!
    //! These assert the corrected [`scan_start_tags`] behavior directly: genuine
    //! JSX `<` inside map callbacks / ternaries / `&&` conditionals / nested
    //! containers is recognized, while TypeScript generics and `<` operators
    //! inside those same containers are still rejected (the generics guarantee is
    //! reused verbatim), interior `}`/`/`/backtick in strings/templates/regex/
    //! comments or nested brackets never close the container early, context
    //! resumes correctly after a container, and an unterminated container at EOF
    //! is the same `ParseFailure` as pre-fix.
    //!
    //! Lives in its own module so it never collides with the other scanner test
    //! modules.

    use super::{scan_start_tags, ParseFailure, ScannedTag};
    use crate::dialect::{Dialect, ScannerProfile};

    fn jsx() -> ScannerProfile {
        Dialect::Jsx.scanner_profile()
    }

    fn scan(src: &str) -> Vec<ScannedTag<'_>> {
        scan_start_tags(src, &jsx(), 2).expect("scan should succeed")
    }

    fn names(src: &str) -> Vec<&str> {
        scan(src).into_iter().map(|t| t.name).collect()
    }

    fn scan_err(src: &str) -> ParseFailure {
        scan_start_tags(src, &jsx(), 2).expect_err("scan should fail with a ParseFailure")
    }

    // ----- genuine JSX recognized inside each container shape -------------

    #[test]
    fn immediate_jsx_in_container_is_recognized() {
        assert_eq!(names("{<Item a b c/>}"), vec!["Item"]);
    }

    #[test]
    fn map_callback_jsx_is_recognized() {
        assert_eq!(names("{x.map(y => <Row p q r/>)}"), vec!["Row"]);
    }

    #[test]
    fn ternary_jsx_is_recognized() {
        assert_eq!(names("{c ? <A m n/> : <B/>}"), vec!["A", "B"]);
    }

    #[test]
    fn and_conditional_jsx_is_recognized() {
        assert_eq!(names("{c && <P q r/>}"), vec!["P"]);
    }

    #[test]
    fn nested_containers_recognize_both_levels() {
        assert_eq!(
            names("{r.map(x => <Row>{x.map(y => <Cell/>)}</Row>)}"),
            vec!["Row", "Cell"]
        );
    }

    // ----- generics inside a container still rejected ---------------------

    #[test]
    fn generics_inside_container_produce_no_tag() {
        for src in [
            "{useState<Array<Task>>()}",
            "{Record<K,V>}",
            "{Omit<T,id>}",
            "{axios.get<Array<X>>()}",
            "{a < b}",
            "{Array<T>.length}",
        ] {
            assert!(
                names(src).is_empty(),
                "`{src}` must produce no tag, got: {:?}",
                names(src)
            );
        }
    }

    // ----- interior object literal `}` not mistaken for the boundary ------

    #[test]
    fn interior_object_literal_brace_is_not_the_boundary() {
        // The object `}` must not close the container early; no tag here.
        assert!(
            names("{x.map(y => ({ a: 1 }))}").is_empty(),
            "the interior object `}}` must not close the container early"
        );
        // And a genuine `<A/>` after an interior object literal is recognized.
        assert_eq!(names("{cond ? <A/> : ({ a: 1 })}"), vec!["A"]);
    }

    // ----- interior token discipline (string/template/regex/comment) ------

    #[test]
    fn interior_string_brace_does_not_close_container() {
        assert!(
            names("{f(\"}\")}").is_empty(),
            "the `}}` inside a string literal must not close the container early"
        );
    }

    #[test]
    fn interior_template_brace_does_not_close_container() {
        assert!(
            names("{`a}b`}").is_empty(),
            "the `}}` inside a template literal must not close the container early"
        );
    }

    #[test]
    fn interior_regex_brace_does_not_close_container() {
        assert!(
            names("{re.test(/a}b/)}").is_empty(),
            "the `}}` inside a regex literal must not close the container early"
        );
    }

    #[test]
    fn interior_block_comment_brace_does_not_close_container() {
        // `/* } */` then a genuine `<Item/>` after the comment is recognized.
        assert_eq!(names("{/* } */ <Item/>}"), vec!["Item"]);
    }

    #[test]
    fn interior_line_comment_brace_does_not_close_container() {
        // The `}` inside a `// ...` line comment must not close the container.
        // The `<Item/>` sits at expression position (after `=>`), so it is
        // recognized once the comment is skipped as a whole token.
        assert_eq!(names("{list.map(x => // }\n<Item/>)}"), vec!["Item"]);
    }

    // ----- context resume after a container -------------------------------

    #[test]
    fn sibling_after_container_is_recognized() {
        assert_eq!(
            names("<ul>{items.map(i => <li/>)}<li>last</li></ul>"),
            vec!["ul", "li", "li"]
        );
    }

    // ----- component-with-generics head inside a container ----------------

    #[test]
    fn component_generics_head_in_container_is_unconfident() {
        let tags = scan("{<EditableList<SearchEngineDomain> a=\"1\">}");
        assert_eq!(
            tags.iter().map(|t| t.name).collect::<Vec<_>>(),
            vec!["EditableList"]
        );
        assert!(
            !tags[0].confidently_rewritable,
            "Change 3 still fires inside a container: the head is a per-tag no-op"
        );
    }

    // ----- unterminated container at EOF ----------------------------------

    #[test]
    fn unterminated_container_at_eof_is_parse_failure() {
        let failure = scan_err("<div>{x.map(");
        assert_eq!(
            failure.message, "unterminated braced expression at end of input",
            "an unterminated child container at EOF is the same hard failure as pre-fix"
        );
    }
}
