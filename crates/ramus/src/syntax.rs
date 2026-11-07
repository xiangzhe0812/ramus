//! A small, language-aware colouring of source lines for the diff view. It is a tokenizer,
//! not a parser: each line is coloured on its own, so a string or comment that spans lines
//! is only right on the line where it starts. That is the right trade for reading diffs.

use std::ops::Range;

/// What kind of text a stretch of a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Keyword,
    String,
    /// A `CamelCase` name, usually a type.
    Type,
    /// A name directly followed by `(`.
    Function,
    Comment,
}

/// The languages worth colouring. Anything else is shown plain: guessing keywords in prose
/// or data files would colour ordinary words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    JavaScript,
    Python,
    Go,
    C,
    Java,
    Shell,
    /// Strings and `#` comments only (TOML, YAML, JSON and similar).
    Data,
    Plain,
}

impl Language {
    pub fn for_path(path: &str) -> Self {
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = match name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
            _ => String::new(),
        };
        match extension.as_str() {
            "rs" => Self::Rust,
            "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => Self::JavaScript,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "m" | "mm" => Self::C,
            "java" | "kt" | "kts" | "swift" | "cs" => Self::Java,
            "sh" | "bash" | "zsh" => Self::Shell,
            "toml" | "yml" | "yaml" | "json" | "ini" | "cfg" => Self::Data,
            _ => Self::Plain,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            Self::JavaScript => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "else",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "from",
                "function",
                "if",
                "import",
                "in",
                "interface",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "type",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "yield",
            ],
            Self::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "if", "import",
                "in", "is", "lambda", "None", "not", "or", "pass", "raise", "return", "True",
                "try", "while", "with", "yield",
            ],
            Self::Go => &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "false",
                "for",
                "func",
                "go",
                "if",
                "import",
                "interface",
                "map",
                "nil",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "true",
                "type",
                "var",
            ],
            Self::C => &[
                "break",
                "case",
                "char",
                "class",
                "const",
                "continue",
                "default",
                "do",
                "double",
                "else",
                "enum",
                "extern",
                "false",
                "float",
                "for",
                "if",
                "include",
                "int",
                "long",
                "namespace",
                "new",
                "nullptr",
                "return",
                "short",
                "signed",
                "sizeof",
                "static",
                "struct",
                "switch",
                "template",
                "this",
                "true",
                "typedef",
                "union",
                "unsigned",
                "using",
                "void",
                "while",
            ],
            Self::Java => &[
                "abstract",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "else",
                "enum",
                "extends",
                "false",
                "final",
                "finally",
                "for",
                "fun",
                "func",
                "if",
                "implements",
                "import",
                "in",
                "interface",
                "let",
                "new",
                "null",
                "override",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "static",
                "struct",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "val",
                "var",
                "void",
                "while",
            ],
            Self::Shell => &[
                "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
                "local", "return", "then", "until", "while",
            ],
            Self::Data | Self::Plain => &[],
        }
    }

    /// The marker that starts a comment running to the end of the line, if any.
    fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::JavaScript | Self::Go | Self::C | Self::Java => Some("//"),
            Self::Python | Self::Shell | Self::Data => Some("#"),
            Self::Plain => None,
        }
    }

    /// Rust uses `'` for lifetimes and chars, so a lone quote there is not a string.
    fn single_quote_strings(self) -> bool {
        !matches!(self, Self::Rust | Self::Plain)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The coloured stretches of `line`, as byte ranges in order and never overlapping.
/// Everything not listed is plain text.
pub fn highlight(line: &str, language: Language) -> Vec<(Range<usize>, Token)> {
    if language == Language::Plain {
        return Vec::new();
    }
    let keywords = language.keywords();
    let comment = language.line_comment();
    let mut tokens = Vec::new();
    let mut chars = line.char_indices().peekable();

    while let Some((start, c)) = chars.next() {
        // A comment runs to the end of the line.
        if let Some(marker) = comment
            && line[start..].starts_with(marker)
        {
            tokens.push((start..line.len(), Token::Comment));
            return tokens;
        }

        let quote = c == '"'
            || c == '`' && language == Language::JavaScript
            || c == '\'' && language.single_quote_strings();
        if quote {
            let mut end = line.len();
            let mut escaped = false;
            for (at, next) in chars.by_ref() {
                if escaped {
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == c {
                    end = at + next.len_utf8();
                    break;
                }
            }
            tokens.push((start..end, Token::String));
            continue;
        }

        if is_ident_start(c) {
            let mut end = start + c.len_utf8();
            while let Some(&(at, next)) = chars.peek() {
                if !is_ident_part(next) {
                    break;
                }
                end = at + next.len_utf8();
                chars.next();
            }
            let word = &line[start..end];
            let followed_by_call = line[end..].starts_with('(');
            let token = if keywords.contains(&word) {
                Some(Token::Keyword)
            } else if followed_by_call {
                Some(Token::Function)
            } else if word.starts_with(char::is_uppercase) && word.chars().any(char::is_lowercase) {
                Some(Token::Type)
            } else {
                None
            };
            if let Some(token) = token {
                tokens.push((start..end, token));
            }
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coloured words, as (text, token), for readable assertions.
    fn marked(line: &str, language: Language) -> Vec<(&str, Token)> {
        highlight(line, language)
            .into_iter()
            .map(|(range, token)| (&line[range], token))
            .collect()
    }

    #[test]
    fn rust_keywords_types_and_calls_are_told_apart() {
        assert_eq!(
            marked("pub fn open(path: PathBuf) -> Repo {", Language::Rust),
            [
                ("pub", Token::Keyword),
                ("fn", Token::Keyword),
                ("open", Token::Function),
                ("PathBuf", Token::Type),
                ("Repo", Token::Type),
            ]
        );
    }

    #[test]
    fn a_string_hides_what_looks_like_code_inside_it() {
        assert_eq!(
            marked(r#"let s = "fn if (x)";"#, Language::Rust),
            [("let", Token::Keyword), (r#""fn if (x)""#, Token::String)]
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        assert_eq!(
            marked(r#"x = "a \" b" + y"#, Language::JavaScript),
            [(r#""a \" b""#, Token::String)]
        );
    }

    #[test]
    fn an_unclosed_string_runs_to_the_end_of_the_line() {
        assert_eq!(
            marked(r#"let s = "never closed"#, Language::Rust),
            [("let", Token::Keyword), (r#""never closed"#, Token::String)]
        );
    }

    #[test]
    fn rust_lifetimes_and_chars_are_not_taken_for_strings() {
        assert_eq!(
            marked("fn f<'a>(x: &'a str) {}", Language::Rust),
            [("fn", Token::Keyword)]
        );
        // But another language's single quotes do start a string.
        assert_eq!(
            marked("x = 'a b'", Language::Python),
            [("'a b'", Token::String)]
        );
    }

    #[test]
    fn a_line_comment_swallows_the_rest_of_the_line() {
        assert_eq!(
            marked("let x = 1; // fn not a keyword", Language::Rust),
            [
                ("let", Token::Keyword),
                ("// fn not a keyword", Token::Comment)
            ]
        );
        assert_eq!(
            marked("# a comment", Language::Python),
            [("# a comment", Token::Comment)]
        );
        // A comment marker inside a string is just text.
        assert_eq!(
            marked(r#""http://x""#, Language::Rust),
            [(r#""http://x""#, Token::String)]
        );
    }

    #[test]
    fn prose_and_unknown_files_are_left_plain() {
        assert!(highlight("if you for in the", Language::Plain).is_empty());
        assert_eq!(Language::for_path("README.md"), Language::Plain);
        assert_eq!(Language::for_path("LICENSE"), Language::Plain);
        assert_eq!(Language::for_path(".gitignore"), Language::Plain);
    }

    #[test]
    fn data_files_colour_strings_and_comments_only() {
        assert_eq!(
            marked(r#"name = "ramus" # the app"#, Language::Data),
            [(r#""ramus""#, Token::String), ("# the app", Token::Comment)]
        );
        // No keywords in data: `for` and `in` stay plain, and `Name(` is still a call-shaped word.
        assert!(marked("for in", Language::Data).is_empty());
    }

    #[test]
    fn the_language_comes_from_the_extension_not_the_folder() {
        assert_eq!(Language::for_path("src/ui/diff_view.rs"), Language::Rust);
        assert_eq!(Language::for_path("web/App.TSX"), Language::JavaScript);
        assert_eq!(Language::for_path("rs/notes"), Language::Plain);
        assert_eq!(Language::for_path("Cargo.toml"), Language::Data);
    }

    #[test]
    fn multi_byte_text_never_splits_a_character() {
        let line = "let 名前 = \"héllo 你好\"; // 注释";
        for (range, _) in highlight(line, Language::Rust) {
            // Slicing would panic if a range cut a character in half.
            let _ = &line[range];
        }
        assert_eq!(
            marked(line, Language::Rust).last(),
            Some(&("// 注释", Token::Comment))
        );
    }

    #[test]
    fn ranges_are_ordered_and_never_overlap() {
        let line = r#"fn Foo(x: Bar) { call("a", 'b'); } // done"#;
        let mut end = 0;
        for (range, _) in highlight(line, Language::Rust) {
            assert!(range.start >= end, "{range:?} overlaps an earlier range");
            end = range.end;
        }
    }
}
