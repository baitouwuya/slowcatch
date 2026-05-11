use anyhow::Result;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn head_lines(path: &Path, count: usize) -> Result<Vec<String>> {
    slice_lines(path, 0, count)
}

pub fn tail_lines(path: &Path, count: usize) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut output = VecDeque::with_capacity(count);

    if count == 0 {
        return Ok(Vec::new());
    }

    for line in reader.lines() {
        if output.len() == count {
            output.pop_front();
        }
        output.push_back(line?);
    }

    Ok(output.into_iter().collect())
}

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

    #[test]
    fn head_reads_first_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        let mut handle = fs::File::create(&file).unwrap();
        for index in 1..=5 {
            writeln!(handle, "line {index}").unwrap();
        }

        let lines = head_lines(&file, 2).unwrap();

        assert_eq!(lines, vec!["line 1", "line 2"]);
    }

    #[test]
    fn tail_reads_last_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        let mut handle = fs::File::create(&file).unwrap();
        for index in 1..=5 {
            writeln!(handle, "line {index}").unwrap();
        }

        let lines = tail_lines(&file, 2).unwrap();

        assert_eq!(lines, vec!["line 4", "line 5"]);
    }
}
