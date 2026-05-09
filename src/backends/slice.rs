use anyhow::Result;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn slice_lines(path: &Path, skip: usize, first: usize) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut output = Vec::new();

    for line in reader.lines().skip(skip).take(first) {
        output.push(line?);
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn slice_reads_exact_line_window() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        let mut handle = fs::File::create(&file).unwrap();
        for index in 1..=5 {
            writeln!(handle, "line {index}").unwrap();
        }

        let lines = slice_lines(&file, 2, 2).unwrap();

        assert_eq!(lines, vec!["line 3", "line 4"]);
    }
}
