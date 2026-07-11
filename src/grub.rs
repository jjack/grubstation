use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::LazyLock;
use regex::Regex;

static MENUENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*menuentry\s+['"]([^'"]+)['"]"#).unwrap());
static SUBMENU_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*submenu\s+['"]([^'"]+)['"]"#).unwrap());

pub fn parse_grub_entries(path: &Path) -> io::Result<Vec<String>> {
    log::info!("Parsing GRUB entries from {:?}", path);
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut entries = Vec::new();
    
    // submenu_stack tracks the nested names of parent submenus (e.g., ["Advanced options for Ubuntu", "Alternative kernels"])
    let mut submenu_stack = Vec::new();
    
    // brace_depth tracks the overall count of open/close curly braces { } in the file
    let mut brace_depth = 0;
    
    // submenu_depths tracks the brace_depth at which each submenu level was declared.
    // When the overall brace_depth falls back to or below this level, we know the submenu block has ended.
    let mut submenu_depths = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if let Some(caps) = SUBMENU_RE.captures(trimmed) {
            // Found a submenu block definition. Extract the submenu title.
            let title = caps.get(1).unwrap().as_str().to_string();
            submenu_stack.push(title);
            // Record the current brace depth BEFORE entering the submenu's curly braces.
            submenu_depths.push(brace_depth);
            if trimmed.contains('{') {
                brace_depth += 1;
            }
        } else if let Some(caps) = MENUENTRY_RE.captures(trimmed) {
            // Found a terminal boot option (menuentry). Extract the title.
            let title = caps.get(1).unwrap().as_str().to_string();
            let mut full_path = submenu_stack.clone();
            full_path.push(title);
            // Join nested names with '>' (e.g., "Advanced options for Ubuntu > Ubuntu with Linux 6.2")
            entries.push(full_path.join(">"));
            if trimmed.contains('{') {
                brace_depth += 1;
            }
        } else {
            // If it's a generic command line (like kernel options or module loading),
            // scan character-by-character to accurately track brace nesting levels.
            for c in trimmed.chars() {
                if c == '{' {
                    brace_depth += 1;
                } else if c == '}' {
                    brace_depth -= 1;
                    // If we've exited a brace block, check if it was the end of our current submenu level.
                    if let Some(&depth) = submenu_depths.last() {
                        if brace_depth <= depth {
                            submenu_stack.pop();
                            submenu_depths.pop();
                        }
                    }
                }
            }
        }
    }

    log::info!("Successfully parsed {} GRUB entries.", entries.len());
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_simple_grub() -> io::Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(file, "menuentry 'Ubuntu' {{")?;
        writeln!(file, "  linux /boot/vmlinuz")?;
        writeln!(file, "}}")?;
        writeln!(file, "submenu 'Advanced' {{")?;
        writeln!(file, "  menuentry 'Kernel 1' {{")?;
        writeln!(file, "    linux /boot/vmlinuz-1")?;
        writeln!(file, "  }}")?;
        writeln!(file, "}}")?;

        let entries = parse_grub_entries(file.path())?;
        assert_eq!(entries, vec!["Ubuntu", "Advanced>Kernel 1"]);
        Ok(())
    }

    #[test]
    fn test_nested_submenus() -> io::Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(file, "submenu 'A' {{")?;
        writeln!(file, "  submenu 'B' {{")?;
        writeln!(file, "    menuentry 'C' {{")?;
        writeln!(file, "      echo 1")?;
        writeln!(file, "    }}")?;
        writeln!(file, "  }}")?;
        writeln!(file, "  menuentry 'D' {{")?;
        writeln!(file, "    echo 2")?;
        writeln!(file, "  }}")?;
        writeln!(file, "}}")?;

        let entries = parse_grub_entries(file.path())?;
        assert_eq!(entries, vec!["A>B>C", "A>D"]);
        Ok(())
    }
}
