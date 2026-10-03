//! POSIX shell quoting helpers.
//!
//! Every remote command we build is ultimately parsed by a POSIX shell (`sh`/`bash`
//! over SSH). Use these helpers instead of ad-hoc `replace('\'', ...)` so that
//! arbitrary values (paths, passwords, generated scripts) survive intact.
//!
//! Rules of thumb:
//! - Interpolate any untrusted / non-literal value as a single word with [`quote`].
//!   The result already includes the surrounding single quotes, so never wrap it in
//!   extra quotes in a format string.
//! - To run a multi-line script through a nested shell (e.g. `sudo bash -lc <script>`),
//!   build the script first (quoting values inside it with [`quote`]) and then quote the
//!   whole script once, e.g. with [`bash_lc`]. Do not hand-write `bash -lc '...{}...'`.
//!
//! Windows / PowerShell quoting is a different concern and lives next to its callers.

/// Quote `value` as a single POSIX shell word.
///
/// The value is wrapped in single quotes, and each embedded `'` is emitted as
/// `'"'"'` (close quote, double-quoted `'`, reopen quote). Inside single quotes the
/// shell performs no expansion at all, so `$`, `` ` ``, `\`, `!`, `*`, `#`, newlines
/// and unicode are all preserved literally. An empty value becomes `''`.
pub fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Build `bash -lc <script>` with `script` quoted as a single word.
///
/// Prefix with `sudo ` / `sudo -u user ` etc. as needed by the caller.
pub fn bash_lc(script: &str) -> String {
    format!("bash -lc {}", quote(script))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: &[&str] = &[
        "",
        "plain",
        "with space",
        "it's",
        "''",
        "double \"quote\"",
        "$HOME ${HOME} $(id)",
        "back`tick`",
        "# not a comment",
        "bang! !!",
        "back\\slash \\n",
        "line1\nline2\n",
        "glob * ? [a]",
        "unicode: héllo – 日本語 🚀",
        "p@ss'w\"o$r`d\\!#*\n end",
        "-leading-dash",
        "semi; rm -rf / && echo | cat > x < y",
    ];

    #[test]
    fn quote_wraps_and_escapes() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("abc"), "'abc'");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\"'\"'s'");
        assert_eq!(quote("$x `y` \\z"), "'$x `y` \\z'");
        assert_eq!(quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn bash_lc_quotes_whole_script() {
        assert_eq!(bash_lc("echo 'hi'"), "bash -lc 'echo '\"'\"'hi'\"'\"''");
    }

    #[cfg(unix)]
    fn run_sh(command: &str) -> Vec<u8> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("failed to run sh");
        assert!(
            output.status.success(),
            "sh failed for {command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[cfg(unix)]
    #[test]
    fn quote_round_trips_through_sh() {
        for sample in SAMPLES {
            let stdout = run_sh(&format!("printf %s {}", quote(sample)));
            assert_eq!(
                String::from_utf8(stdout).unwrap(),
                *sample,
                "sample {sample:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn quote_round_trips_through_nested_shell() {
        let has_bash = std::process::Command::new("bash")
            .arg("-c")
            .arg("true")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        for sample in SAMPLES {
            let script = format!("printf %s {}", quote(sample));
            // Fall back to a plain nested `sh -c` where bash is unavailable.
            let command = if has_bash {
                bash_lc(&script)
            } else {
                format!("sh -c {}", quote(&script))
            };
            let stdout = run_sh(&command);
            assert_eq!(
                String::from_utf8(stdout).unwrap(),
                *sample,
                "sample {sample:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn quoted_heredoc_inside_nested_shell_preserves_content() {
        let content = "password = p@ss'w\"o$r`d\\!#*\nkey = 'x'";
        let script = format!("cat <<\"EOF\"\n{content}\nEOF");
        let stdout = run_sh(&format!("sh -c {}", quote(&script)));
        assert_eq!(String::from_utf8(stdout).unwrap(), format!("{content}\n"));
    }
}
