use std::fs;
use std::process::Command;

#[test]
fn cli_slice_outputs_raw_lines() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("sample.txt");
    fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_slowcatch"))
        .arg("slice")
        .arg(&file)
        .arg("--skip")
        .arg("1")
        .arg("--first")
        .arg("2")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "two\nthree\n");
}

#[test]
fn cli_grep_outputs_path_line_text() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("sample.txt");
    fs::write(&file, "one\nneedle\nthree\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_slowcatch"))
        .arg("grep")
        .arg("needle")
        .arg(temp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sample.txt:2:needle"));
}

#[test]
fn cli_slice_outputs_json_records() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("sample.txt");
    fs::write(&file, "one\ntwo\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_slowcatch"))
        .arg("slice")
        .arg(&file)
        .arg("--skip")
        .arg("0")
        .arg("--first")
        .arg("1")
        .arg("--output")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json[0]["line_number"], 1);
    assert_eq!(json[0]["text"], "one");
}

#[test]
fn cli_grep_supports_jsonl_select_where_sort_limit() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("b.rs");
    let second = temp.path().join("a.rs");
    let ignored = temp.path().join("c.txt");
    fs::write(&first, "needle b\n").unwrap();
    fs::write(&second, "needle a\n").unwrap();
    fs::write(&ignored, "needle txt\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_slowcatch"))
        .arg("grep")
        .arg("needle")
        .arg(temp.path())
        .arg("--output")
        .arg("jsonl")
        .arg("--where")
        .arg("extension=.rs")
        .arg("--sort-by")
        .arg("name")
        .arg("--limit")
        .arg("1")
        .arg("--select")
        .arg("name,line")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["name"], "a.rs");
    assert_eq!(json["line"], "needle a");
    assert!(json.get("path").is_none());
}
