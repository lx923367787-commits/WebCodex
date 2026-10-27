//! Lexical rules shared by private-file admission and assignment issuance.
//! Filesystem handles must still enforce links and actual ownership.
pub fn component_valid(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 128
        || matches!(value, "." | "..")
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return false;
    }
    let folded = value.to_ascii_lowercase();
    if folded.starts_with(".snapshot-") {
        return false;
    }
    let base = folded.split('.').next().unwrap_or("");
    !matches!(base, "con" | "prn" | "aux" | "nul" | "clock$")
        && !(base.len() == 4
            && (base.starts_with("com") || base.starts_with("lpt"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

#[cfg(test)]
mod tests {
    use super::component_valid;

    #[test]
    fn assignment_and_file_components_share_windows_alias_boundaries() {
        for name in [
            "README.md",
            "Cargo.toml",
            ".gitignore",
            ".config",
            "task-one",
        ] {
            assert!(component_valid(name), "{name}");
        }
        for name in [
            "",
            ".",
            "..",
            "NUL.txt",
            "con",
            "COM1.log",
            "Lpt9",
            "x.",
            "x ",
            "x:y",
            "a/b",
            "a\\b",
            ".SNAPSHOT-anything",
        ] {
            assert!(!component_valid(name), "{name}");
        }
    }
}
