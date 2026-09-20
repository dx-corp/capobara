use std::collections::HashMap;

use regex::Regex;

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Default)]
pub struct PathOpts {
    pub pattern: bool,
    pub root: bool,
}

pub fn has_wildcard(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '*' | '?' | '[' | ']'))
}

fn unsafe_path(path: &str) -> Error {
    Error::Contract(format!(
        "Unsafe projection path: {}",
        serde_json::to_string(path).unwrap_or_default()
    ))
}

pub fn safe_path(path: &str, opts: PathOpts) -> Result<&str> {
    if opts.root && path == "." {
        return Ok(path);
    }
    let bad_char = path
        .chars()
        .any(|c| c == '\\' || c == ':' || (c as u32) < 0x20 || c as u32 == 0x7f);
    if path.is_empty() || path.starts_with('/') || bad_char {
        return Err(unsafe_path(path));
    }
    if path
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
    {
        return Err(unsafe_path(path));
    }
    if !opts.pattern && has_wildcard(path) {
        return Err(unsafe_path(path));
    }
    Ok(path)
}

struct Rule {
    regex: Regex,
    prefix: Option<String>,
}

pub struct Matcher {
    rules: Vec<Rule>,
}

impl Matcher {
    pub fn new(patterns: &[String]) -> Result<Matcher> {
        let mut rules = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            safe_path(
                pattern,
                PathOpts {
                    pattern: true,
                    root: false,
                },
            )?;
            let mut source = String::from("^");
            let mut chars = pattern.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '*' if chars.peek() == Some(&'*') => {
                        chars.next();
                        source.push_str(".*");
                    }
                    '*' => source.push_str("[^/]*"),
                    '.' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']'
                    | '\\' => {
                        source.push('\\');
                        source.push(c);
                    }
                    other => source.push(other),
                }
            }
            source.push('$');
            let regex = Regex::new(&source)
                .map_err(|e| Error::Invalid(format!("Bad pattern {pattern}: {e}")))?;
            let prefix = pattern.strip_suffix("/**").map(str::to_owned);
            rules.push(Rule { regex, prefix });
        }
        Ok(Matcher { rules })
    }

    pub fn matches(&self, path: &str) -> bool {
        self.rules
            .iter()
            .any(|r| r.prefix.as_deref() == Some(path) || r.regex.is_match(path))
    }
}

pub fn assert_portable_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut prefixes: HashMap<String, String> = HashMap::new();
    for path in paths {
        safe_path(path, PathOpts::default())?;
        let parts: Vec<&str> = path.split('/').collect();
        for length in 1..=parts.len() {
            let prefix = parts[..length].join("/");
            let key = prefix.to_lowercase();
            match prefixes.get(&key) {
                Some(existing) if existing != &prefix => {
                    return Err(Error::Contract(format!(
                        "Case-colliding projection path: {path}"
                    )));
                }
                Some(_) => {}
                None => {
                    prefixes.insert(key, prefix);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_rejects_traversal_git_and_control_chars() {
        for bad in [
            "",
            "/abs",
            "a/../b",
            "a/./b",
            ".git/config",
            "A/.GIT/x",
            "a\\b",
            "a:b",
            "a\u{7f}b",
            "a\nb",
            "glob*",
        ] {
            assert!(
                safe_path(bad, PathOpts::default()).is_err(),
                "{bad:?} accepted"
            );
        }
        assert_eq!(
            safe_path("src/lib.rs", PathOpts::default()).unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            safe_path(
                ".",
                PathOpts {
                    root: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            "."
        );
        assert!(safe_path(".", PathOpts::default()).is_err());
        assert_eq!(
            safe_path(
                "src/**",
                PathOpts {
                    pattern: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            "src/**"
        );
    }

    #[test]
    fn matcher_star_is_one_segment_and_doublestar_crosses() {
        let m = Matcher::new(&["src/*.rs".into(), "docs/**".into(), "LICENSE".into()]).unwrap();
        assert!(m.matches("src/lib.rs"));
        assert!(!m.matches("src/a/lib.rs"));
        assert!(m.matches("docs/a/b/c.md"));
        assert!(m.matches("docs"));
        assert!(!m.matches("docs2"));
        assert!(m.matches("LICENSE"));
        assert!(!m.matches("LICENSE.txt"));
    }

    #[test]
    fn matcher_escapes_regex_metacharacters() {
        let m = Matcher::new(&["a.b".into()]).unwrap();
        assert!(m.matches("a.b"));
        assert!(!m.matches("aXb"));
    }

    #[test]
    fn portable_paths_reject_case_aliases_at_any_depth() {
        assert!(assert_portable_paths(["Src/a.rs", "src/b.rs"]).is_err());
        assert!(assert_portable_paths(["README.md", "readme.md"]).is_err());
        assert!(assert_portable_paths(["a/b", "a/c", "d"]).is_ok());
    }
}
