//! Shell quoting helpers for transport adapters.

/// Quote one value for a POSIX shell.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Join an exact argv vector into a shell-quoted command string.
#[must_use]
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_single_quotes_without_interpolation() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
