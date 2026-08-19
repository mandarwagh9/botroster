//! No message reaches a person carrying source indentation.
//!
//! A Rust string literal that spans source lines keeps the newline and the
//! leading whitespace of the next line unless it ends with a backslash. The
//! result is a sentence with thirty spaces in the middle of it, and it is
//! invisible in review because the source looks neatly aligned:
//!
//! ```text
//!   Routines pause after 14 days with nobody watching,                       so an agent...
//! ```
//!
//! The defect can land in a printed message, an empty-state hint, or a
//! template written to disk (as `openbot skill new` does), where it corrupts
//! a file rather than a line of output.
//!
//! The correct form is a trailing backslash, which strips both the newline and
//! the indentation. Content with real newlines should be built from lines and
//! joined, not written as one literal.
//!
//! The defect has a second form. Once anything joins the literal back onto
//! one line (a formatter, an editor, a script writing the file) the newline
//! disappears and the indentation stays, mid-sentence, in a line of source
//! that looks entirely normal. Both forms are checked below.

use std::path::Path;

/// Walk a Rust source file and yield its ordinary string literals.
///
/// Escapes are neutralised, including `\` + newline: that is the correct
/// form and must not be reported. Char literals are skipped, because `'"'`
/// would otherwise open a phantom string and throw off the rest of the file.
fn literals(src: &str) -> Vec<(usize, String)> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;

    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        // Comments: prose, often with quotes in it.
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        // Char literals.
        if c == '\'' {
            if i + 3 < b.len() && b[i + 1] == '\\' {
                i += 2;
                while i < b.len() && b[i] != '\'' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            if i + 2 < b.len() && b[i + 2] == '\'' {
                i += 3;
                continue;
            }
        }
        // Raw strings say what they mean.
        if c == 'r' && i + 1 < b.len() && (b[i + 1] == '"' || b[i + 1] == '#') {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < b.len() && b[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == '"' {
                j += 1;
                let close: String = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                let rest: String = b[j..].iter().collect();
                let end = rest.find(&close).map(|k| j + rest[..k].chars().count());
                let stop = end.unwrap_or(b.len());
                line += b[i..stop].iter().filter(|c| **c == '\n').count();
                i = stop + close.chars().count();
                continue;
            }
        }
        if c == '"' {
            let start_line = line;
            let mut j = i + 1;
            let mut text = String::new();
            while j < b.len() {
                if b[j] == '\\' {
                    // A continuation: Rust drops the newline and the indent,
                    // so drop them here too.
                    if j + 1 < b.len() && b[j + 1] == '\n' {
                        line += 1;
                        j += 2;
                        while j < b.len() && b[j] == ' ' {
                            j += 1;
                        }
                        continue;
                    }
                    j += 2;
                    continue;
                }
                if b[j] == '"' {
                    break;
                }
                if b[j] == '\n' {
                    line += 1;
                }
                text.push(b[j]);
                j += 1;
            }
            out.push((start_line, text));
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn scan(dir: &Path, problems: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // `target` is generated and enormous.
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            scan(&p, problems);
            continue;
        }
        if p.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&p) else {
            continue;
        };
        // This repository checks out with CRLF on Windows, so a line
        // continuation is `\` + CR + LF. Without normalising, every correctly
        // continued literal in the tree would look like a leak.
        let src = src.replace("\r\n", "\n");

        for (line, text) in literals(&src) {
            // A newline followed by real indentation: source formatting that
            // will be read by a person.
            let carries_indent = text
                .find('\n')
                .is_some_and(|at| text[at + 1..].starts_with("   "));

            // The same defect once the literal has been joined onto one line.
            // The newline that gave it away is gone and the source looks
            // ordinary, so the check above walks straight past it.
            //
            // Column padding must not be flagged: `"model     {}"` and
            // `"Bug   Repro"` are intentional. Both pad to something (a
            // placeholder or a capitalised heading), so requiring lowercase on
            // each side leaves them alone.
            let bytes = text.as_bytes();
            let collapsed = text.match_indices("   ").any(|(i, _)| {
                let before = bytes[..i].last();
                let rest = &text[i..];
                // How long the run actually is. `match_indices` yields a hit
                // at every offset inside a long run; only the first has a real
                // character before it, so the rest fail the `before` test on
                // their own.
                let run = rest.len() - rest.trim_start_matches(' ').len();
                let after = rest.trim_start_matches(' ').as_bytes().first();
                // A sentence can break at punctuation as easily as at a word
                // ("reports no usage,        so the budget"), so punctuation
                // counts on the `before` side. What keeps column padding safe
                // is the other side: padding pads to a placeholder or a
                // capitalised heading, never to a lowercase word, so requiring
                // one there is the check that discriminates.
                //
                // A backtick opens or closes a word as often as a letter does
                // (this codebase names things in `like_this` everywhere), so
                // it counts on both sides: "served;              `fs.read`"
                // and "`connector add`                with" are both the
                // defect. Nothing intentional pads to or from a backtick.
                let lower = matches!(before, Some(c) if c.is_ascii_lowercase() || b",.;:`".contains(c))
                    && matches!(after, Some(c) if c.is_ascii_lowercase() || *c == b'`');

                // Length is the discriminator the character rule lacks.
                // Requiring lowercase on the far side misses every
                // continuation that breaks at a sentence ("...instead.
                // <indent> Write it as..."), which is common in error text
                // and tool descriptions.
                //
                // Padding pads by a few columns; source indentation here is
                // eight to twenty-five. At eight and above, only baked
                // continuations occur; `"Bug   Repro"`, the intentional
                // padding this rule protects, sits at three.
                let indent_width = run >= 8
                    && matches!(before, Some(c) if c.is_ascii_alphanumeric() || b",.;:`".contains(c))
                    && matches!(after, Some(c) if c.is_ascii_alphabetic() || *c == b'`');

                lower || indent_width
            });

            if carries_indent || collapsed {
                problems.push(format!(
                    "{}:{line}: {}",
                    p.display(),
                    text.replace('\n', "\\n")
                        .chars()
                        .take(70)
                        .collect::<String>()
                ));
            }
        }
    }
}

#[test]
fn no_message_carries_the_indentation_of_the_code_that_wrote_it() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut problems = Vec::new();
    scan(&crates, &mut problems);

    assert!(
        problems.is_empty(),
        "these string literals will print with this file's indentation in them.\n\
         End each line with a backslash, or build the text from lines and join:\n  {}",
        problems.join("\n  ")
    );
}
