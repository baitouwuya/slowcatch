use crate::backends::everything::{EverythingFinder, FileFinder, FindQuery, TimedEverythingFinder};
use crate::backends::grep::{self, GrepQuery};
use crate::backends::slice;
use crate::integrations::codex_hook;
use crate::structured::{
    OutputFormat, RecordOptions, WhereClause, apply_options, grep_record, parse_select,
    parse_sort_by, path_record, render_records, slice_record, validate_columns,
};
use anyhow::Result;
use clap::{Args as ClapArgs, Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

const STRUCTURED_SCAN_LIMIT: usize = 5_000;

#[derive(Debug, Parser)]
#[command(name = "slowcatch")]
#[command(about = "High-performance local developer utilities")]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Find {
        pattern: String,
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 80)]
        limit: usize,
        #[command(flatten)]
        records: RecordArgs,
    },
    Grep {
        pattern: String,
        roots: Vec<PathBuf>,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[command(flatten)]
        records: RecordArgs,
    },
    Slice {
        path: PathBuf,
        #[arg(long, default_value_t = 0)]
        skip: usize,
        #[arg(long)]
        first: usize,
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        records: RecordArgs,
    },
    Hook {
        #[command(subcommand)]
        integration: HookIntegration,
    },
    SelfTest,
}

#[derive(Debug, Subcommand)]
enum HookIntegration {
    Codex,
}

#[derive(Debug, Clone, ClapArgs)]
struct RecordArgs {
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
    #[arg(long)]
    select: Option<String>,
    #[arg(long = "where")]
    where_clause: Option<String>,
    #[arg(long)]
    sort_by: Option<String>,
}

impl RecordArgs {
    fn to_options(&self, limit: Option<usize>) -> Result<RecordOptions> {
        Ok(RecordOptions {
            select: self.select.as_deref().map(parse_select).transpose()?,
            where_clause: self
                .where_clause
                .as_deref()
                .map(WhereClause::parse)
                .transpose()?,
            sort_by: self.sort_by.as_deref().map(parse_sort_by).transpose()?,
            limit,
        })
    }

    fn needs_expanded_scan(&self) -> bool {
        self.where_clause.is_some() || self.sort_by.is_some()
    }
}

pub fn run() -> Result<()> {
    match Args::parse().command {
        Command::Find {
            pattern,
            root,
            limit,
            records,
        } => {
            let backend_limit = backend_limit(limit, records.needs_expanded_scan());
            let paths = EverythingFinder.find(&FindQuery {
                root,
                pattern,
                files_only: true,
                limit: backend_limit,
            })?;
            let options = records.to_options(Some(limit))?;
            validate_columns(&options, &["path", "name", "extension", "parent"])?;
            let output_records = apply_options(
                paths.into_iter().map(|path| path_record(&path)).collect(),
                &options,
            );
            print_records(&output_records, records.output, Some("path"))
        }
        Command::Grep {
            pattern,
            roots,
            case_sensitive,
            limit,
            records,
        } => {
            let backend_limit = backend_limit(limit, records.needs_expanded_scan());
            let matches = grep::grep(&GrepQuery {
                roots,
                pattern,
                case_sensitive,
                simple_match: true,
                limit: backend_limit,
            })?;
            let options = records.to_options(Some(limit))?;
            validate_columns(
                &options,
                &["path", "name", "extension", "parent", "line_number", "line"],
            )?;
            let output_records = apply_options(
                matches
                    .into_iter()
                    .map(|item| grep_record(&item.path, item.line_number, &item.line))
                    .collect(),
                &options,
            );
            print_records(&output_records, records.output, None)
        }
        Command::Slice {
            path,
            skip,
            first,
            limit,
            records,
        } => {
            let lines = slice::slice_lines(&path, skip, first)?;
            let options = records.to_options(limit)?;
            validate_columns(&options, &["line_number", "text"])?;
            let output_records = apply_options(
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(index, line)| slice_record(skip + index + 1, &line))
                    .collect(),
                &options,
            );
            print_records(&output_records, records.output, Some("text"))
        }
        Command::Hook {
            integration: HookIntegration::Codex,
        } => codex_hook::run_from_stdin(),
        Command::SelfTest => {
            crate::shell_parse::parse_powershell(
                "Get-ChildItem -Recurse -Filter Cargo.toml",
                std::env::current_dir()?.to_str(),
            )
            .ok_or_else(|| anyhow::anyhow!("PowerShell parser self-test failed"))?;
            let finder = TimedEverythingFinder::new(Duration::from_secs(3));
            let _ = finder.find(&FindQuery {
                root: std::env::current_dir()?.to_str().map(ToOwned::to_owned),
                pattern: "Cargo.toml".to_string(),
                files_only: true,
                limit: 1,
            })?;
            println!("slowcatch self-test passed");
            Ok(())
        }
    }
}

fn backend_limit(limit: usize, needs_expanded_scan: bool) -> usize {
    if needs_expanded_scan {
        limit.max(STRUCTURED_SCAN_LIMIT)
    } else {
        limit
    }
}

fn print_records(
    records: &[crate::structured::Record],
    output: OutputFormat,
    text_column: Option<&str>,
) -> Result<()> {
    let rendered = render_records(records, output, text_column)?;
    if !rendered.is_empty() {
        println!("{rendered}");
    }
    Ok(())
}
