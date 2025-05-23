use crate::{
    commands::patterns_test::filter_patterns_by_regex,
    error::GoodError,
    flags::GlobalFormatFlags,
    messenger_variant::create_emitter,
    resolver::{resolve_from_cwd, Source},
};
use anyhow::{anyhow, Context, Result};
use biome_grit_formatter::context::GritFormatOptions;
use clap::Args;
use colored::Colorize;
use marzano_core::api::{DoneFile, MatchResult, Rewrite};
use marzano_gritmodule::{config::ResolvedGritDefinition, parser::PatternFileExt};
use marzano_language::{markdown_block::MarkdownBlock, target_language::TargetLanguage};
use marzano_messenger::{
    emit::{ApplyDetails, Messager},
    output_mode::OutputMode,
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Args, Debug, Serialize, Clone)]
pub struct FormatGritArgs {
    /// Write formats to file instead of just showing them
    #[clap(long)]
    pub write: bool,
    // Level of detail to show for results
    #[clap(
        long = "output",
        default_value_t = OutputMode::Standard,
    )]
    output: OutputMode,
    /// Regex of a specific pattern to test
    #[clap(long = "filter")]
    pub filter: Option<String>,
}

pub async fn run_format(arg: &FormatGritArgs, flags: &GlobalFormatFlags) -> Result<()> {
    let (mut resolved, _) = resolve_from_cwd(&Source::Local).await?;

    if let Some(filter) = &arg.filter {
        resolved = filter_patterns_by_regex(resolved, filter)?;
    }

    let file_path_to_resolved = group_resolved_patterns_by_group(resolved);

    if file_path_to_resolved.is_empty() {
        return Err(anyhow!("No patterns found to format"));
    }

    println!(
        "Formatting {} {}",
        format!("{}", file_path_to_resolved.len()).bold().yellow(),
        if file_path_to_resolved.len() == 1 {
            "pattern"
        } else {
            "patterns"
        }
    );

    // Create an emitter for formatting results
    let mut emitter = create_emitter(
        &crate::flags::OutputFormat::from(flags),
        arg.output.clone(),
        None,
        false,
        None,
        None,
        marzano_messenger::emit::VisibilityLevels::default(),
    )
    .await?;

    let mut details = ApplyDetails {
        matched: 0,
        rewritten: 0,
        named_pattern: None,
    };
    let dry_run = !arg.write;

    for (file_path, resolved_patterns) in file_path_to_resolved {
        let results = format_file_resolved_patterns(&file_path, resolved_patterns);
        if let Ok(results) = results {
            emitter.handle_results(
                results,
                &mut details,
                dry_run,
                false,
                &mut false,
                None,
                None,
                None,
                &TargetLanguage::MarkdownBlock(MarkdownBlock::new(None)),
            );
        }
    }

    if details.rewritten > 0 {
        println!(
            "{} formatting changes in {} {}",
            if arg.write { "Made" } else { "Found" },
            format!("{}", details.rewritten).bold().yellow(),
            if details.rewritten == 1 {
                "file"
            } else {
                "files"
            }
        );
        anyhow::bail!(GoodError::new());
    }

    println!("No formatting changes were made");
    Ok(())
}

fn group_resolved_patterns_by_group(
    resolved: Vec<ResolvedGritDefinition>,
) -> Vec<(String, Vec<ResolvedGritDefinition>)> {
    let mut map = BTreeMap::new();

    // Group into map
    for resolved in resolved {
        map.entry(resolved.config.path.clone())
            .or_insert_with(Vec::new)
            .push(resolved);
    }

    // Convert to Vec
    map.into_iter().collect()
}

fn format_file_resolved_patterns(
    file_path: &str,
    patterns: Vec<ResolvedGritDefinition>,
) -> Result<Vec<MatchResult>> {
    // Apply patterns in reverse order to avoid conflicts
    let mut patterns = patterns;
    patterns.sort_by_key(|p| std::cmp::Reverse(p.config.range.map_or(0, |r| r.start_byte)));

    let first_pattern = patterns
        .first()
        .ok_or_else(|| anyhow!("patterns is empty"))?;
    let first_pattern_raw_data = first_pattern
        .config
        .raw
        .as_ref()
        .ok_or_else(|| anyhow!("pattern doesn't have raw data"))?;

    let format = first_pattern_raw_data.format;
    let old_file_content = &first_pattern_raw_data.content;

    let mut results = vec![];

    let new_file_content = match format {
        PatternFileExt::Yaml => {
            let (this_results, new_file_content) =
                yaml::apply_yaml_rewrites(&patterns, old_file_content)?;
            results.extend(this_results);
            new_file_content
        }
        PatternFileExt::Grit => {
            let (this_results, new_file_content) = format_grit_code(old_file_content)?;
            results.extend(this_results);
            new_file_content
        }
        PatternFileExt::Md => {
            let mut new_file_content = old_file_content.clone();
            for pattern in &patterns {
                if let Some(range) = pattern.config.range {
                    let (this_results, formatted_pattern) = format_grit_code(&pattern.body)
                        .with_context(|| format!("could not format '{}'", pattern.name()))?;
                    results.extend(this_results);
                    new_file_content.replace_range(
                        range.start_byte as usize..range.end_byte as usize,
                        formatted_pattern.as_str(),
                    );
                } else {
                    println!("pattern {} has no range", pattern.name());
                }
            }
            new_file_content
        }
    };

    results.push(MatchResult::DoneFile(DoneFile::new(file_path.to_owned())));

    if &new_file_content == old_file_content {
        return Ok(results);
    }

    results.push(MatchResult::Rewrite(Rewrite::for_file(
        file_path,
        old_file_content,
        &new_file_content,
    )));

    Ok(results)
}

/// format grit code using `biome`
fn format_grit_code(source: &str) -> Result<(Vec<MatchResult>, String)> {
    let result = std::panic::catch_unwind(|| biome_grit_parser::parse_grit(source));

    let Ok(parsed) = result else {
        return Err(anyhow!("Syntax error in grit code, parsing failed"));
    };

    let options = GritFormatOptions::default();
    let doc = biome_grit_formatter::format_node(options, &parsed.syntax())
        .with_context(|| "biome couldn't format")?;
    let formatted = doc.print()?.into_code();
    Ok((vec![], formatted))
}

mod yaml {
    use std::collections::HashMap;

    use anyhow::{anyhow, bail, Context, Result};
    use grit_pattern_matcher::pattern::{
        Contains, DynamicPattern, FilePattern, Pattern, Rewrite as PatternRewrite, StringConstant,
    };
    use marzano_core::api::MatchResult;
    use marzano_core::sdk::LanguageSdk;
    use marzano_gritmodule::config::ResolvedGritDefinition;
    use marzano_language::target_language::PatternLanguage;
    use marzano_util::{rich_path::RichFile, runtime::ExecutionContext};

    use super::format_grit_code;

    pub(crate) fn apply_yaml_rewrites(
        patterns: &[ResolvedGritDefinition],
        file_content: &str,
    ) -> Result<(Vec<MatchResult>, String)> {
        let mut results = vec![];
        let mut sdk = LanguageSdk::from_language(PatternLanguage::Yaml.try_into().unwrap());

        // Build pattern for each grit definition
        let pattern_rewrites = patterns
            .iter()
            .map(|pattern| {
                let (this_results, formatted_body) = format_grit_code(&pattern.body)
                    .with_context(|| format!("could not format '{}'", pattern.name()))?;
                results.extend(this_results);

                // Create the pattern name matcher
                let name_pair = sdk.compiler().node(
                    "block_mapping_pair",
                    HashMap::from([
                        (
                            "key",
                            Pattern::StringConstant(StringConstant::new("name".to_owned())),
                        ),
                        (
                            "value",
                            Pattern::StringConstant(StringConstant::new(pattern.name().to_owned())),
                        ),
                    ]),
                )?;

                // Create the body replacement
                let body_pair = sdk.compiler().node(
                    "block_mapping_pair",
                    HashMap::from([
                        (
                            "key",
                            Pattern::StringConstant(StringConstant::new("body".to_owned())),
                        ),
                        (
                            "value",
                            Pattern::Rewrite(Box::new(PatternRewrite::new(
                                Pattern::Top,
                                DynamicPattern::from_str_constant(&format_yaml_body_code(
                                    &formatted_body,
                                ))?,
                                None,
                            ))),
                        ),
                    ]),
                )?;

                // Combine into a block mapping
                let block = sdk.compiler().node(
                    "block_mapping",
                    HashMap::from([(
                        "items",
                        Pattern::And(Box::new(grit_pattern_matcher::pattern::And::new(vec![
                            Pattern::Some(Box::new(grit_pattern_matcher::pattern::Some::new(
                                name_pair,
                            ))),
                            Pattern::Some(Box::new(grit_pattern_matcher::pattern::Some::new(
                                body_pair,
                            ))),
                        ]))),
                    )]),
                )?;

                Ok(block)
            })
            .collect::<Result<Vec<_>>>()?;

        // Combine all patterns into a single file pattern
        let file_pattern = Pattern::File(Box::new(FilePattern::new(
            Pattern::Top,
            Pattern::And(Box::new(grit_pattern_matcher::pattern::And::new(
                pattern_rewrites
                    .into_iter()
                    .map(|p| Contains::new_pattern(p, None))
                    .collect(),
            ))),
        )));

        let compiled = sdk.build(file_pattern)?;
        let rich_file = RichFile::new(String::new(), file_content.to_owned());
        let runtime = ExecutionContext::default();

        for result in compiled.execute_file(&rich_file, &runtime) {
            if let MatchResult::Rewrite(rewrite) = result {
                let content = rewrite
                    .rewritten
                    .content
                    .ok_or_else(|| anyhow!("rewritten content is empty"))?;
                return Ok((results, content));
            }
        }

        bail!("no rewrite result after applying pattern")
    }

    fn format_yaml_body_code(input: &str) -> String {
        // yaml body still needs two indentation to look good
        let body_with_prefix = prefix_lines(input, &" ".repeat(2));
        let escaped_body = body_with_prefix.replace("\"", "\\\"");
        // body: |
        //   escaped_body
        format!("|\n{escaped_body}")
    }

    fn prefix_lines(input: &str, prefix: &str) -> String {
        input
            .lines()
            .map(|line| {
                if line.is_empty() {
                    line.to_owned()
                } else {
                    format!("{prefix}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_format_go_imports() -> Result<()> {
        // This somehow has a massive memory leak but only in --release mode
        // See https://github.com/biomejs/biome/issues/5032

        let fixtures_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("cli_bin")
            .join("fixtures")
            .join("go")
            .join("imports.grit");

        let input = std::fs::read_to_string(&fixtures_path)?;
        let result = format_grit_code(&input);

        println!("done: {:?}", result);

        Ok(())
    }
}
