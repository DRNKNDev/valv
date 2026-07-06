pub fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_escape_escapes_backslashes_and_quotes() {
        assert_eq!(
            toml_escape(r#"path\with"quote"#),
            r#"path\\with\"quote"#
        );
    }
}
