//! `clanka recall` and `clanka eval`: looking into what a query brings up, and
//! measuring it over a file of queries, to tune `[recall]` in config.toml
//! against a real companion. Both only read.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Paths, SharedEmbedder};
use clanka_core::{Companion, Recalled};
use serde::Deserialize;
use std::path::Path;

fn open(paths: &Paths, soul: &Path) -> Result<Companion> {
    setup::open(paths, soul, SharedEmbedder::load(paths)?, None)
}

/// What one query brings up, best first, with the numbers behind the order.
pub fn recall(paths: &Paths, soul: &Path, query: &str) -> Result<()> {
    let companion = open(paths, soul)?;
    let found = companion.search(query, companion.ranking())?;
    for r in &found {
        println!(
            "{:.2} relevance, {:.2} score  #{} [{}, {}] {}",
            r.relevance,
            r.score,
            r.memory.id,
            r.memory.kind.as_str(),
            r.memory.level.as_str(),
            r.memory.interpretation
        );
    }
    let ranking = companion.ranking();
    println!(
        "{} of at most {} memories (min_relevance {}, min_score {}).",
        found.len(),
        ranking.limit,
        ranking.min_relevance,
        ranking.min_score
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    case: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// What the user would say.
    query: String,
    /// Texts that should come up: each must appear, in any letter case, in a
    /// recalled memory's interpretation or facts.
    #[serde(default)]
    expect: Vec<String>,
    /// Texts that should not come up at all.
    #[serde(default)]
    reject: Vec<String>,
}

/// Each recalled memory as the text a case is matched against: its
/// interpretation and the values of its facts, one per line, in lower case.
/// The values only: as JSON, a brace or a fact's name would match too.
fn texts(found: &[Recalled]) -> Vec<String> {
    found
        .iter()
        .map(|r| {
            let mut text = r.memory.interpretation.clone();
            for value in r.memory.facts.values() {
                text.push('\n');
                match value.as_str() {
                    Some(said) => text.push_str(said),
                    None => text.push_str(&value.to_string()),
                }
            }
            text.to_lowercase()
        })
        .collect()
}

/// A cases file that would pass whatever recall does: no case, a case that
/// expects and rejects nothing, or an empty text, which every memory holds.
fn check(cases: &Cases) -> Result<()> {
    if cases.case.is_empty() {
        bail!("no cases");
    }
    for case in &cases.case {
        if case.expect.is_empty() && case.reject.is_empty() {
            bail!(
                "the case {:?} has neither `expect` nor `reject`",
                case.query
            );
        }
        if case
            .expect
            .iter()
            .chain(&case.reject)
            .any(|t| t.trim().is_empty())
        {
            bail!(
                "the case {:?} has an empty text in `expect` or `reject`",
                case.query
            );
        }
    }
    Ok(())
}

/// The 1-based position of the first recalled memory that holds `text`.
fn rank(found: &[String], text: &str) -> Option<usize> {
    let text = text.to_lowercase();
    found.iter().position(|m| m.contains(&text)).map(|i| i + 1)
}

/// The tally over every case: (expected texts found, expected in all, the sum
/// of 1/rank over the found ones, rejected texts that came up).
#[derive(Debug, Default, PartialEq)]
struct Tally {
    found: usize,
    expected: usize,
    reciprocal_ranks: f64,
    unwanted: usize,
}

fn tally(case: &Case, found: &[String], tally: &mut Tally, lines: &mut Vec<String>) {
    for text in &case.expect {
        tally.expected += 1;
        match rank(found, text) {
            Some(rank) => {
                tally.found += 1;
                tally.reciprocal_ranks += 1.0 / rank as f64;
                lines.push(format!("  ok    {text:?} at {rank}"));
            }
            None => lines.push(format!("  MISS  {text:?}")),
        }
    }
    for text in &case.reject {
        if let Some(rank) = rank(found, text) {
            tally.unwanted += 1;
            lines.push(format!("  NOISE {text:?} at {rank}"));
        }
    }
}

/// Runs every query in a cases file and says how much of what should come up
/// did: `false` on a miss or on noise, which the exit status passes on, so a
/// script can gate on it.
pub fn eval(paths: &Paths, soul: &Path, cases: &Path) -> Result<bool> {
    let text =
        std::fs::read_to_string(cases).with_context(|| format!("reading {}", cases.display()))?;
    let parsed: Cases =
        toml::from_str(&text).with_context(|| format!("reading {}", cases.display()))?;
    check(&parsed).with_context(|| format!("reading {}", cases.display()))?;
    let cases = parsed;
    let companion = open(paths, soul)?;
    let mut total = Tally::default();
    for case in &cases.case {
        let found = companion.search(&case.query, companion.ranking())?;
        let mut lines = Vec::new();
        tally(case, &texts(&found), &mut total, &mut lines);
        println!("{:?}: {} recalled", case.query, found.len());
        for line in lines {
            println!("{line}");
        }
    }
    let mean = if total.found == 0 {
        0.0
    } else {
        total.reciprocal_ranks / total.expected as f64
    };
    println!(
        "\n{} of {} expected memories came up (mean reciprocal rank {mean:.2}); {} unwanted.",
        total.found, total.expected, total.unwanted
    );
    Ok(total.found == total.expected && total.unwanted == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_cases_file_that_cannot_fail_is_refused() {
        for file in [
            "case = []\n",
            "[[case]]\nquery = \"the concert\"\n",
            "[[case]]\nquery = \"the concert\"\nexpect = [\" \"]\n",
        ] {
            assert!(check(&toml::from_str(file).unwrap()).is_err(), "{file}");
        }
        let fine = "[[case]]\nquery = \"the concert\"\nreject = [\"work\"]\n";
        assert!(check(&toml::from_str(fine).unwrap()).is_ok());
    }

    #[test]
    fn a_case_counts_what_came_up_and_where() {
        let cases: Cases = toml::from_str(
            "[[case]]\nquery = \"the concert\"\nexpect = [\"LAURA\", \"dad\"]\nreject = [\"work\"]\n",
        )
        .unwrap();
        let found = [
            "stress at work".to_owned(),
            "laura sang at the concert".to_owned(),
        ];
        let (mut total, mut lines) = (Tally::default(), Vec::new());
        tally(&cases.case[0], &found, &mut total, &mut lines);
        assert_eq!(
            total,
            Tally {
                found: 1,
                expected: 2,
                reciprocal_ranks: 0.5,
                unwanted: 1
            }
        );
        assert_eq!(
            lines,
            [
                "  ok    \"LAURA\" at 2",
                "  MISS  \"dad\"",
                "  NOISE \"work\" at 1"
            ]
        );
    }
}
