//! POSIX shell quoting, for the one place a command must become a string:
//! the command line an SSH server hands to the remote login shell.

/// `s` as one POSIX shell word.
pub fn word(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./=:@%+,".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `argv` as a POSIX shell command line.
pub fn line<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|a| word(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_what_needs_it() {
        assert_eq!(word("plain/path-1.txt"), "plain/path-1.txt");
        assert_eq!(word(""), "''");
        assert_eq!(word("a b"), "'a b'");
        assert_eq!(word("it's"), r"'it'\''s'");
        assert_eq!(word("$(x)"), "'$(x)'");
        assert_eq!(line(&["echo", "a;b"]), "echo 'a;b'");
    }

    #[test]
    fn a_shell_reads_back_what_was_quoted() {
        let nasty = [
            "a b",
            "it's",
            "$(touch /tmp/x)",
            "`x`",
            "\\",
            "\n",
            "*",
            "-n",
            "",
        ];
        let script = format!("printf '%s\\0' {}", line(&nasty));
        let out = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        let got: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect();
        assert_eq!(&got[..nasty.len()], &nasty);
    }
}
