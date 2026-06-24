use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use regex::Regex;

pub fn parse_grub_entries(path: &Path) -> io::Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut entries = Vec::new();
    let mut submenu_stack = Vec::new();
    let mut brace_depth = 0;
    let mut submenu_depths = Vec::new();

    // Regex to match menuentry 'Title' ... {
    let menuentry_re = Regex::new(r#"^\s*menuentry\s+['"]([^'"]+)['"]"#).unwrap();
    // Regex to match submenu 'Title' ... {
    let submenu_re = Regex::new(r#"^\s*submenu\s+['"]([^'"]+)['"]"#).unwrap();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if let Some(caps) = submenu_re.captures(trimmed) {
            let title = caps.get(1).unwrap().as_str().to_string();
            submenu_stack.push(title);
            submenu_depths.push(brace_depth);
            if trimmed.contains('{') {
                brace_depth += 1;
            }
        } else if let Some(caps) = menuentry_re.captures(trimmed) {
            let title = caps.get(1).unwrap().as_str().to_string();
            let mut full_path = submenu_stack.clone();
            full_path.push(title);
            entries.push(full_path.join(">"));
            if trimmed.contains('{') {
                brace_depth += 1;
            }
        } else {
            // Track brace depth to know when a submenu ends
            for c in trimmed.chars() {
                if c == '{' {
                    brace_depth += 1;
                } else if c == '}' {
                    brace_depth -= 1;
                    if let Some(&depth) = submenu_depths.last() {
                        if brace_depth == depth {
                            submenu_stack.pop();
                            submenu_depths.pop();
                        }
                    }
                }
            }
        }
    }

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
