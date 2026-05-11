use crate::backends::everything::{FileFinder, FindQuery};
use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;

const MAX_UUIDS: usize = 5;
const SINGLE_UUID_PATH_LIMIT: usize = 5;
const MULTI_UUID_PATH_LIMIT: usize = 3;
const BACKEND_QUERY_LIMIT: usize = 20;
const MAX_CONTEXT_BYTES: usize = 8 * 1024;

pub trait PromptEnricher {
    fn name(&self) -> &'static str;
    fn enrich(&self, prompt: &str, finder: &dyn FileFinder) -> Result<Option<PromptReferences>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReferences {
    pub groups: Vec<ReferenceGroup>,
    pub debug_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceGroup {
    pub kind: String,
    pub key: String,
    pub items: Vec<String>,
    pub total: ReferenceTotal,
    pub debug_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceTotal {
    Exact(usize),
    AtLeast(usize),
}

impl ReferenceTotal {
    fn render(&self) -> String {
        match self {
            ReferenceTotal::Exact(value) => value.to_string(),
            ReferenceTotal::AtLeast(value) => format!("{value}+"),
        }
    }
}

pub struct UuidPathLookupDetector;

pub fn enrich_prompt(prompt: &str, finder: &dyn FileFinder) -> Option<String> {
    let enrichers: [&dyn PromptEnricher; 1] = [&UuidPathLookupDetector];
    enrich_prompt_with_enrichers(prompt, finder, &enrichers)
}

pub fn enrich_prompt_with_enrichers(
    prompt: &str,
    finder: &dyn FileFinder,
    enrichers: &[&dyn PromptEnricher],
) -> Option<String> {
    let mut references = Vec::new();
    for enricher in enrichers {
        match enricher.enrich(prompt, finder) {
            Ok(Some(reference)) => references.push((enricher.name(), reference)),
            Ok(None) | Err(_) => {}
        }
    }
    render_references(&references)
}

impl PromptEnricher for UuidPathLookupDetector {
    fn name(&self) -> &'static str {
        "uuid_path_lookup"
    }

    fn enrich(&self, prompt: &str, finder: &dyn FileFinder) -> Result<Option<PromptReferences>> {
        let detected = detect_uuid_like_values(prompt);
        if detected.values.is_empty() {
            return Ok(None);
        }

        let per_uuid_limit = if detected.values.len() == 1 {
            SINGLE_UUID_PATH_LIMIT
        } else {
            MULTI_UUID_PATH_LIMIT
        };
        let mut debug_lines = vec![
            "search=Everything path/name only".to_string(),
            "files_read=false".to_string(),
        ];
        if detected.truncated {
            debug_lines.push(format!(
                "uuid_limit={MAX_UUIDS} reached; later UUID-like values were skipped"
            ));
        }

        let mut groups = Vec::new();
        for uuid in detected.values {
            let paths = finder.find(&FindQuery {
                root: None,
                pattern: uuid.clone(),
                files_only: true,
                limit: BACKEND_QUERY_LIMIT,
            })?;
            if paths.is_empty() {
                continue;
            }

            let shown = paths.len().min(per_uuid_limit);
            let total = if paths.len() >= BACKEND_QUERY_LIMIT {
                ReferenceTotal::AtLeast(BACKEND_QUERY_LIMIT)
            } else {
                ReferenceTotal::Exact(paths.len())
            };
            let mut group_debug = vec![format!(
                "uuid={uuid} matches={} showing={shown}",
                total.render()
            )];
            if paths.len() > per_uuid_limit {
                group_debug.push(format!(
                    "omitted_at_least={}",
                    paths.len().saturating_sub(per_uuid_limit)
                ));
            }
            groups.push(ReferenceGroup {
                kind: "uuid".to_string(),
                key: uuid,
                items: paths.into_iter().take(per_uuid_limit).collect(),
                total,
                debug_lines: group_debug,
            });
        }

        if groups.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PromptReferences {
                groups,
                debug_lines,
            }))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UuidDetection {
    values: Vec<String>,
    truncated: bool,
}

fn detect_uuid_like_values(prompt: &str) -> UuidDetection {
    let regex = Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("valid UUID regex");
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    let mut truncated = false;
    for matched in regex.find_iter(prompt) {
        if has_uuid_continuation(prompt, matched.start(), matched.end()) {
            continue;
        }
        let value = matched.as_str();
        if !seen.insert(value.to_ascii_lowercase()) {
            continue;
        }
        if values.len() >= MAX_UUIDS {
            truncated = true;
            break;
        }
        values.push(value.to_string());
    }
    UuidDetection { values, truncated }
}

fn has_uuid_continuation(prompt: &str, start: usize, end: usize) -> bool {
    previous_char(prompt, start).is_some_and(is_uuid_continuation_char)
        || next_char(prompt, end).is_some_and(is_uuid_continuation_char)
}

fn previous_char(value: &str, index: usize) -> Option<char> {
    value[..index].chars().next_back()
}

fn next_char(value: &str, index: usize) -> Option<char> {
    value[index..].chars().next()
}

fn is_uuid_continuation_char(value: char) -> bool {
    value.is_ascii_hexdigit() || value == '-'
}

fn render_references(references: &[(&'static str, PromptReferences)]) -> Option<String> {
    render_references_with_debug(references, prompt_debug_enabled())
}

fn render_references_with_debug(
    references: &[(&'static str, PromptReferences)],
    debug_enabled: bool,
) -> Option<String> {
    if references.is_empty() {
        return None;
    }

    let mut output = String::new();
    let mut truncated = false;
    push_line_capped(&mut output, "slowcatch_refs v1 paths_only", &mut truncated);
    if debug_enabled {
        push_line_capped(&mut output, "debug=1", &mut truncated);
    }
    for (name, reference) in references {
        if debug_enabled {
            push_line_capped(&mut output, "", &mut truncated);
            push_line_capped(&mut output, &format!("detector={name}"), &mut truncated);
            for line in &reference.debug_lines {
                push_line_capped(&mut output, line, &mut truncated);
            }
        }
        for group in &reference.groups {
            push_line_capped(
                &mut output,
                &format!(
                    "{}:{} [{}/{}]",
                    group.kind,
                    group.key,
                    group.items.len(),
                    group.total.render()
                ),
                &mut truncated,
            );
            for path in &group.items {
                push_line_capped(&mut output, path, &mut truncated);
            }
            if debug_enabled {
                for line in &group.debug_lines {
                    push_line_capped(&mut output, line, &mut truncated);
                }
            }
        }
    }
    if truncated {
        let _ = append_line_if_fits(&mut output, "truncated=context byte cap reached");
    }
    Some(output)
}

fn prompt_debug_enabled() -> bool {
    std::env::var("SLOWCATCH_PROMPT_DEBUG").is_ok_and(|value| value == "1")
}

fn push_line_capped(output: &mut String, line: &str, truncated: &mut bool) {
    if *truncated {
        return;
    }
    if !append_line_if_fits(output, line) {
        *truncated = true;
    }
}

fn append_line_if_fits(output: &mut String, line: &str) -> bool {
    let extra = line.len() + usize::from(!output.is_empty());
    if output.len() + extra > MAX_CONTEXT_BYTES {
        return false;
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(line);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct QueryRecordingFinder {
        paths: Vec<String>,
        queries: Mutex<Vec<FindQuery>>,
    }

    impl QueryRecordingFinder {
        fn new(paths: Vec<String>) -> Self {
            Self {
                paths,
                queries: Mutex::new(Vec::new()),
            }
        }

        fn query_count(&self) -> usize {
            self.queries.lock().unwrap().len()
        }
    }

    impl FileFinder for QueryRecordingFinder {
        fn find(&self, query: &FindQuery) -> Result<Vec<String>> {
            self.queries.lock().unwrap().push(query.clone());
            Ok(self.paths.clone())
        }
    }

    struct FailingFinder;

    impl FileFinder for FailingFinder {
        fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
            anyhow::bail!("backend unavailable")
        }
    }

    #[test]
    fn prompt_without_uuid_has_no_enrichment() {
        let finder = QueryRecordingFinder::new(vec![]);

        let output = enrich_prompt("hello world", &finder);

        assert!(output.is_none());
        assert_eq!(finder.query_count(), 0);
    }

    #[test]
    fn single_uuid_outputs_up_to_five_paths() {
        let uuid = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(
            (0..8)
                .map(|index| format!("C:\\repo\\file-{index}-{uuid}.jsonl"))
                .collect(),
        );

        let output = enrich_prompt(&format!("look up {uuid}"), &finder).unwrap();

        assert!(output.starts_with("slowcatch_refs v1 paths_only"));
        assert!(output.contains(&format!("uuid:{uuid} [5/8]")));
        assert_eq!(output.lines().filter(|line| line.contains(uuid)).count(), 6);
        assert_no_verbose_fields(&output);
        assert_eq!(finder.query_count(), 1);
    }

    #[test]
    fn uuid_followed_by_chinese_text_is_detected() {
        let uuid = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(vec![
            "C:\\repo\\rollout-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl".to_string(),
        ]);

        let output = enrich_prompt(&format!("{uuid}测试"), &finder).unwrap();

        assert!(output.contains(&format!("uuid:{uuid} [1/1]")));
        assert_eq!(finder.query_count(), 1);
    }

    #[test]
    fn uuid_embedded_in_longer_hex_or_hyphen_sequence_is_ignored() {
        let uuid = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(vec![
            "C:\\repo\\rollout-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl".to_string(),
        ]);

        assert!(enrich_prompt(&format!("a{uuid}"), &finder).is_none());
        assert!(enrich_prompt(&format!("{uuid}-tail"), &finder).is_none());
        assert_eq!(finder.query_count(), 0);
    }

    #[test]
    fn multiple_uuids_output_up_to_three_paths_each_and_dedupe() {
        let uuid_a = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let uuid_b = "119e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(
            (0..8)
                .map(|index| format!("C:\\repo\\file-{index}.jsonl"))
                .collect(),
        );

        let output = enrich_prompt(&format!("{uuid_a} {uuid_b} {uuid_a}"), &finder).unwrap();

        assert!(output.contains(&format!("uuid:{uuid_a} [3/8]")));
        assert!(output.contains(&format!("uuid:{uuid_b} [3/8]")));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("C:\\repo\\file-"))
                .count(),
            6
        );
        assert_no_verbose_fields(&output);
        assert_eq!(finder.query_count(), 2);
    }

    #[test]
    fn more_than_five_uuids_are_truncated_without_verbose_output() {
        let prompt = (0..6)
            .map(|index| format!("{index:08x}-4f63-7bc1-8d24-0586a9098481"))
            .collect::<Vec<_>>()
            .join(" ");
        let finder = QueryRecordingFinder::new(vec!["C:\\repo\\match.jsonl".to_string()]);

        let output = enrich_prompt(&prompt, &finder).unwrap();

        assert!(!output.contains("uuid_limit=5"));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("uuid:"))
                .count(),
            5
        );
        assert_eq!(finder.query_count(), 5);
    }

    #[test]
    fn capped_backend_count_renders_with_plus_suffix() {
        let uuid = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(
            (0..BACKEND_QUERY_LIMIT)
                .map(|index| format!("C:\\repo\\file-{index}-{uuid}.jsonl"))
                .collect(),
        );

        let output = enrich_prompt(&format!("look up {uuid}"), &finder).unwrap();

        assert!(output.contains(&format!("uuid:{uuid} [5/{BACKEND_QUERY_LIMIT}+]")));
        assert_no_verbose_fields(&output);
    }

    #[test]
    fn uuid_with_no_matches_produces_no_enrichment() {
        let uuid = "019e0b7c-4f63-7bc1-8d24-0586a9098481";
        let finder = QueryRecordingFinder::new(vec![]);

        let output = enrich_prompt(&format!("look up {uuid}"), &finder);

        assert!(output.is_none());
        assert_eq!(finder.query_count(), 1);
    }

    #[test]
    fn backend_failure_fails_open() {
        let output = enrich_prompt("019e0b7c-4f63-7bc1-8d24-0586a9098481", &FailingFinder);

        assert!(output.is_none());
    }

    fn assert_no_verbose_fields(output: &str) {
        for field in [
            "detector=",
            "search=",
            "files_read=",
            "matches=",
            "showing=",
            "omitted_at_least=",
        ] {
            assert!(
                !output.contains(field),
                "{field} should not be in compact output"
            );
        }
    }

    struct StaticEnricher {
        title: &'static str,
        group: ReferenceGroup,
        debug_lines: Vec<String>,
    }

    impl PromptEnricher for StaticEnricher {
        fn name(&self) -> &'static str {
            self.title
        }

        fn enrich(
            &self,
            _prompt: &str,
            _finder: &dyn FileFinder,
        ) -> Result<Option<PromptReferences>> {
            Ok(Some(PromptReferences {
                groups: vec![self.group.clone()],
                debug_lines: self.debug_lines.clone(),
            }))
        }
    }

    struct FailingEnricher;

    impl PromptEnricher for FailingEnricher {
        fn name(&self) -> &'static str {
            "failing"
        }

        fn enrich(
            &self,
            _prompt: &str,
            _finder: &dyn FileFinder,
        ) -> Result<Option<PromptReferences>> {
            anyhow::bail!("expected test failure")
        }
    }

    #[test]
    fn independent_enricher_failure_does_not_hide_successful_enricher() {
        let finder = QueryRecordingFinder::new(vec![]);
        let failing = FailingEnricher;
        let successful = StaticEnricher {
            title: "static",
            group: ReferenceGroup {
                kind: "static".to_string(),
                key: "ok".to_string(),
                items: vec!["C:\\repo\\ok.txt".to_string()],
                total: ReferenceTotal::Exact(1),
                debug_lines: vec!["line=ok".to_string()],
            },
            debug_lines: vec!["section=ok".to_string()],
        };

        let output =
            enrich_prompt_with_enrichers("prompt", &finder, &[&failing, &successful]).unwrap();

        assert!(output.contains("static:ok [1/1]"));
        assert!(output.contains("C:\\repo\\ok.txt"));
        assert!(!output.contains("line=ok"));
        assert!(!output.contains("detector=failing"));
    }

    #[test]
    fn rendered_context_is_capped() {
        let finder = QueryRecordingFinder::new(vec![]);
        let long = StaticEnricher {
            title: "long",
            group: ReferenceGroup {
                kind: "long".to_string(),
                key: "key".to_string(),
                items: vec!["x".repeat(MAX_CONTEXT_BYTES * 2)],
                total: ReferenceTotal::Exact(1),
                debug_lines: vec![],
            },
            debug_lines: vec![],
        };

        let output = enrich_prompt_with_enrichers("prompt", &finder, &[&long]).unwrap();

        assert!(output.len() <= MAX_CONTEXT_BYTES);
        assert!(output.contains("truncated=context byte cap reached"));
    }

    #[test]
    fn debug_rendering_includes_detector_diagnostics() {
        let references = PromptReferences {
            groups: vec![ReferenceGroup {
                kind: "uuid".to_string(),
                key: "019e0b7c-4f63-7bc1-8d24-0586a9098481".to_string(),
                items: vec!["C:\\repo\\match.jsonl".to_string()],
                total: ReferenceTotal::Exact(1),
                debug_lines: vec![
                    "uuid=019e0b7c-4f63-7bc1-8d24-0586a9098481 matches=1 showing=1".to_string(),
                ],
            }],
            debug_lines: vec![
                "search=Everything path/name only".to_string(),
                "files_read=false".to_string(),
            ],
        };

        let output = render_references_with_debug(&[("uuid_path_lookup", references)], true)
            .expect("debug output");

        assert!(output.contains("slowcatch_refs v1 paths_only"));
        assert!(output.contains("debug=1"));
        assert!(output.contains("detector=uuid_path_lookup"));
        assert!(output.contains("search=Everything path/name only"));
        assert!(output.contains("files_read=false"));
        assert!(output.contains("matches=1 showing=1"));
    }
}
