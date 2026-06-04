//! LongMemEval dataset loader + bundled synthetic fixture.
//!
//! The schema mirrors the public LongMemEval JSON files: each question
//! ships its full haystack inline, plus the gold session ids that
//! actually contain the answer. Distractor sessions live in the same
//! list with no special marker — recall is purely "did the retriever
//! surface a gold session".

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub question_id: String,
    pub question: String,
    /// Free-text expected answer — not scored, kept for trace logs.
    #[serde(default)]
    pub answer: String,
    pub haystack: Vec<Session>,
    pub answer_session_ids: Vec<String>,
}

pub fn load_from_file<P: AsRef<Path>>(p: P) -> Result<Vec<Question>> {
    let raw = std::fs::read_to_string(p.as_ref())
        .with_context(|| format!("read dataset {}", p.as_ref().display()))?;
    serde_json::from_str(&raw).context("parse dataset JSON")
}

/// Built-in 12-question fixture. Each question targets one of the
/// LongMemEval question types and is engineered so a clear lexical
/// signal lives in the gold session — BM25 alone should clear 96.6%
/// here. The harness's job is to catch *regressions*, not to validate
/// the headline number against this set.
pub fn synthetic() -> Vec<Question> {
    [
        sq(
            "q1-pref",
            "What kind of tea did I tell you I prefer?",
            "oolong",
            &[
                gold(
                    "g1",
                    &[
                        ("user", "i think oolong tea pairs better with a cold morning than green or black"),
                        ("assistant", "noted — oolong is your morning preference"),
                    ],
                ),
                distractor("d1a", "i was reading about coffee farms in colombia"),
                distractor("d1b", "let's discuss the latest typescript release notes"),
                distractor("d1c", "what's a good camera for portraits at f/1.8"),
            ],
            &["g1"],
        ),
        sq(
            "q2-fact",
            "Where did I say my friend Hadassah was moving to?",
            "Reykjavik",
            &[
                gold(
                    "g2",
                    &[
                        ("user", "my friend hadassah is relocating to reykjavik in september"),
                        ("assistant", "good luck to hadassah on the iceland move"),
                    ],
                ),
                distractor("d2a", "the rust 1.86 release stabilized portable simd flags"),
                distractor("d2b", "consider switching from postgres to clickhouse for events"),
                distractor("d2c", "what are the rules for chess castling"),
            ],
            &["g2"],
        ),
        sq(
            "q3-temporal",
            "Which day of the week did I say I have my dentist appointment?",
            "Thursday",
            &[
                gold(
                    "g3",
                    &[
                        ("user", "remember my dentist appointment is thursday at 9am"),
                        ("assistant", "thursday 9am dentist — got it"),
                    ],
                ),
                distractor("d3a", "i'm thinking of buying a fountain pen"),
                distractor("d3b", "let's review the kubernetes networking model"),
            ],
            &["g3"],
        ),
        sq(
            "q4-update",
            "What did I say my new favourite jogging route is?",
            "Vondelpark loop",
            &[
                distractor("d4a", "i used to jog along the canal in centrum"),
                gold(
                    "g4",
                    &[
                        ("user", "actually my new favourite jogging route is the vondelpark loop"),
                        ("assistant", "vondelpark loop noted as the new favourite"),
                    ],
                ),
                distractor("d4b", "consider stretching the iliotibial band more often"),
            ],
            &["g4"],
        ),
        sq(
            "q5-multi",
            "What programming languages did I tell you my brother is learning?",
            "Rust and Elixir",
            &[
                gold(
                    "g5a",
                    &[
                        ("user", "my brother just started learning rust this month"),
                        ("assistant", "rust is a great pick"),
                    ],
                ),
                gold(
                    "g5b",
                    &[
                        ("user", "my brother also picked up elixir on the side"),
                        ("assistant", "elixir + rust is a solid combo"),
                    ],
                ),
                distractor("d5a", "i'm reading about colour theory in painting"),
                distractor("d5b", "what's the time complexity of merge sort"),
            ],
            &["g5a", "g5b"],
        ),
        sq(
            "q6-lookup",
            "What did I say the project codename for our auth refactor was?",
            "Aurora",
            &[
                gold(
                    "g6",
                    &[
                        ("user", "we're calling the auth refactor project aurora internally"),
                        ("assistant", "project aurora — auth refactor — noted"),
                    ],
                ),
                distractor("d6a", "the meeting room is named saturn"),
                distractor("d6b", "i finished the security review of the payments module"),
            ],
            &["g6"],
        ),
        sq(
            "q7-pref",
            "What did I tell you about my coffee brewing method?",
            "Aeropress",
            &[
                distractor("d7a", "i'm switching to a kalita wave dripper"),
                gold(
                    "g7",
                    &[
                        ("user", "i actually do all my home coffee on an aeropress with paper filters"),
                        ("assistant", "aeropress + paper filters at home, got it"),
                    ],
                ),
                distractor("d7b", "espresso machines need monthly descaling"),
            ],
            &["g7"],
        ),
        sq(
            "q8-update",
            "What city did I say I'm planning my next vacation to?",
            "Lisbon",
            &[
                distractor("d8a", "we used to spend summers in mallorca"),
                gold(
                    "g8",
                    &[
                        ("user", "i'm planning the next vacation to lisbon in october"),
                        ("assistant", "lisbon in october — sounds great"),
                    ],
                ),
            ],
            &["g8"],
        ),
        sq(
            "q9-lookup",
            "What kind of dog did I say my neighbour has?",
            "Bernese mountain dog",
            &[
                distractor("d9a", "the cat upstairs is a british shorthair"),
                gold(
                    "g9",
                    &[
                        ("user", "my neighbour has a huge bernese mountain dog called gus"),
                        ("assistant", "gus the bernese — noted"),
                    ],
                ),
                distractor("d9b", "labradoodles are a poodle/labrador cross"),
            ],
            &["g9"],
        ),
        sq(
            "q10-fact",
            "What library did I say I'd benchmark our markdown parser against?",
            "comrak",
            &[
                gold(
                    "g10",
                    &[
                        ("user", "we'll benchmark our markdown parser against comrak as the baseline"),
                        ("assistant", "comrak as baseline — got it"),
                    ],
                ),
                distractor("d10a", "pulldown-cmark has the lowest binary size"),
                distractor("d10b", "the markdown-rs crate is what we're shipping"),
            ],
            &["g10"],
        ),
        sq(
            "q11-pref",
            "What kind of music did I say I write code to?",
            "ambient post-rock",
            &[
                distractor("d11a", "i used to listen to a lot of late-90s hip-hop"),
                gold(
                    "g11",
                    &[
                        ("user", "lately i write code to ambient post-rock — sigur ros, mogwai"),
                        ("assistant", "ambient post-rock for code sessions, noted"),
                    ],
                ),
                distractor("d11b", "the new aphex twin EP is great for cleaning the kitchen"),
            ],
            &["g11"],
        ),
        sq(
            "q12-update",
            "What did I say I changed my preferred IDE shortcut for jumping to definition to?",
            "F12",
            &[
                distractor("d12a", "i used to use cmd+click for go-to-definition"),
                gold(
                    "g12",
                    &[
                        ("user", "i remapped go-to-definition to F12 across all my editors last week"),
                        ("assistant", "F12 for jump-to-definition — noted"),
                    ],
                ),
                distractor("d12b", "vim users use ctrl+] for tag jumps"),
            ],
            &["g12"],
        ),
    ]
    .into_iter()
    .collect()
}

fn sq(
    qid: &str,
    question: &str,
    answer: &str,
    haystack: &[Session],
    gold_ids: &[&str],
) -> Question {
    Question {
        question_id: qid.into(),
        question: question.into(),
        answer: answer.into(),
        haystack: haystack.to_vec(),
        answer_session_ids: gold_ids.iter().copied().map(str::to_string).collect(),
    }
}

fn gold(id: &str, turns: &[(&str, &str)]) -> Session {
    Session {
        session_id: id.into(),
        turns: turns
            .iter()
            .map(|(r, c)| Turn {
                role: r.to_string(),
                content: c.to_string(),
            })
            .collect(),
    }
}

fn distractor(id: &str, msg: &str) -> Session {
    Session {
        session_id: id.into(),
        turns: vec![
            Turn {
                role: "user".into(),
                content: msg.into(),
            },
            Turn {
                role: "assistant".into(),
                content: "noted".into(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_has_at_least_ten_questions_with_gold() {
        let qs = synthetic();
        assert!(qs.len() >= 10);
        for q in &qs {
            assert!(
                !q.answer_session_ids.is_empty(),
                "{}: no gold",
                q.question_id
            );
            for gid in &q.answer_session_ids {
                assert!(
                    q.haystack.iter().any(|s| &s.session_id == gid),
                    "{}: gold {} missing from haystack",
                    q.question_id,
                    gid
                );
            }
        }
    }
}
