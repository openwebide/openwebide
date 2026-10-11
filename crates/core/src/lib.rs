//! Shared domain types used across the Open WebIDE frontend, backend, and crates.

pub mod assistance;
pub mod questions;
pub use assistance::{AssistanceKind, AssistanceRequest, BackgroundCompletion};
pub mod goal;
pub mod memory;
pub mod plugins;
pub mod skills;
pub use skills::{ProjectSkill, ProjectSkills, SkillCommand, SkillDraft, SkillResource};
pub mod scheduled;
pub use goal::{Goal, GoalCommand, GoalStatus};
pub use memory::{MemoryCommand, ProjectMemories, ProjectMemory};
pub mod chat_queue;
pub mod prompt;
pub mod push;
pub use chat_queue::{QueuedPrompt, QueuedPromptKey};
pub use prompt::{PromptContent, PromptImage};
pub mod context;
pub use context::ContextBreakdown;
pub mod compaction;
pub use compaction::*;
pub mod approval;
pub use approval::*;
pub mod model_setup;
pub use model_setup::*;
pub mod bridge;
pub mod diff;
pub mod editor;
pub mod file_type;
pub mod git;
pub mod highlight;
pub mod host;
pub mod host_admin;
pub mod html;
pub use host::*;
pub mod reviews;
pub use reviews::{ReviewHunk, ReviewPlan, ReviewRequest, RunChange};
pub mod rewind;
pub mod run;
pub use rewind::{RewindFile, RewindPlan};
pub mod search;
pub mod session_export;
pub use session_export::{SessionExport, session_markdown, session_markdown_filename};
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod tool_timing;
pub use tool_timing::ToolTiming;
pub mod tui;
pub mod utf8;
pub mod vfs;

pub use bridge::*;
pub use diff::*;
pub use file_type::*;
pub use git::*;
pub use html::html_to_markdown;
pub use run::*;
pub use tui::*;
pub use vfs::{MemoryVfs, Vfs, VfsError, VfsFuture, format_utc_timestamp, normalize_vfs_path};

pub mod connection;
pub use connection::*;
pub mod chat;
pub use chat::*;
pub mod account;
pub use account::*;
pub mod workspace;
pub use workspace::*;

use serde::{Deserialize, Serialize};

/// A web search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Health/status payload returned by the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_context_preserves_prompt_and_formats_utc() {
        let temporal = "Current Date & Time: Thursday, January 1, 1970 00:00 UTC";
        assert_eq!(with_temporal_context(None, 0), temporal);
        assert_eq!(with_temporal_context(Some("   ".into()), 0), temporal);
        assert_eq!(
            with_temporal_context(Some("coder".into()), 0),
            format!("coder\n\n{temporal}")
        );
    }

    #[test]
    fn run_plan_json_round_trip() {
        let connection: Connection = serde_json::from_value(serde_json::json!({
            "name": "local", "kind": "ollama", "base_url": "http://localhost:11434",
            "model": null, "enabled": true,
        }))
        .unwrap();
        for (kind, json) in [
            (RunKind::Chat, serde_json::json!({"kind": "chat"})),
            (RunKind::WebChat, serde_json::json!({"kind": "web_chat"})),
            (
                RunKind::Agent {
                    project_path: "repos/app".into(),
                },
                serde_json::json!({"kind": "agent", "project_path": "repos/app"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&kind).unwrap(), json);
            let plan = RunPlan {
                plugin_executables: Vec::new(),
                plugin_grants: Default::default(),
                plugin_skills: Vec::new(),
                transport: Default::default(),
                environment: RunEnvironment::default(),
                user_content: "go".into(),
                request: ChatRequest {
                    model_settings: Default::default(),
                    connection_id: 1,
                    system_prompt: None,
                    model: None,
                    messages: vec![],
                    tools: vec![],
                },
                connection: connection.clone(),
                kind,
            };
            let encoded = serde_json::to_value(&plan).unwrap();
            assert_eq!(encoded["kind"], json);
            assert_eq!(serde_json::from_value::<RunPlan>(encoded).unwrap(), plan);
        }
    }

    #[test]
    fn content_matches_finds_case_insensitive_lines() {
        let content = "fn main() {\n    let x = 1;\n    println!(\"Hello\");\n}\nfn MAIN() {}\n";
        let hits = find_content_matches(content, "main");
        assert_eq!(
            hits,
            vec![
                (1, "fn main() {".to_string()),
                (5, "fn MAIN() {}".to_string())
            ]
        );
    }

    #[test]
    fn content_matches_empty_query_matches_every_line() {
        let content = "a\nb\nc";
        assert_eq!(
            find_content_matches(content, ""),
            vec![(1, "a".into()), (2, "b".into()), (3, "c".into())]
        );
    }

    #[test]
    fn content_matches_no_match_is_empty() {
        assert!(find_content_matches("hello world", "zzz").is_empty());
    }

    #[test]
    fn diff_inline_strips_common_prefix_suffix() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb\nc\nd".into()),
            new: "a\nX\nc\nd".into(),
        };
        assert_eq!(
            diff_inline_lines(&diff),
            vec![('-', "b".into()), ('+', "X".into())]
        );
    }

    #[test]
    fn diff_inline_new_file_is_all_additions() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "new.txt".into(),
            old: None,
            new: "a\nb".into(),
        };
        assert_eq!(
            diff_inline_lines(&diff),
            vec![('+', "a".into()), ('+', "b".into())]
        );
    }

    #[test]
    fn diff_inline_full_replacement_lists_all() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb".into()),
            new: "x\ny".into(),
        };
        assert_eq!(
            diff_inline_lines(&diff),
            vec![
                ('-', "a".into()),
                ('-', "b".into()),
                ('+', "x".into()),
                ('+', "y".into())
            ]
        );
    }

    #[test]
    fn diff_side_by_side_aligns_changed_middle() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb\nc".into()),
            new: "a\nx\nc".into(),
        };
        assert_eq!(
            diff_side_by_side(&diff),
            vec![
                (Some("a".into()), Some("a".into())),
                (Some("b".into()), Some("x".into())),
                (Some("c".into()), Some("c".into()))
            ]
        );
    }

    #[test]
    fn diff_side_by_side_new_file_pads_left() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "new.txt".into(),
            old: None,
            new: "a\nb".into(),
        };
        assert_eq!(
            diff_side_by_side(&diff),
            vec![(None, Some("a".into())), (None, Some("b".into()))]
        );
    }

    #[test]
    fn diff_side_by_side_deletion_pads_right() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb\nc".into()),
            new: "a\nc".into(),
        };
        assert_eq!(
            diff_side_by_side(&diff),
            vec![
                (Some("a".into()), Some("a".into())),
                (Some("b".into()), None),
                (Some("c".into()), Some("c".into()))
            ]
        );
    }

    #[test]
    fn diff_side_by_side_detailed_inserted_line_keeps_rest_context() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb\nc".into()),
            new: "x\na\nb\nc".into(),
        };
        let rows = diff_side_by_side_detailed(&diff);
        assert_eq!(rows.len(), 4);
        // The inserted line is the only change; everything below stays
        // aligned context instead of shifting into false pairs.
        assert!(rows[0].0.is_none());
        assert_eq!(rows[0].1.as_ref().unwrap().marker, '+');
        for (left, right) in &rows[1..] {
            let (l, r) = (left.as_ref().unwrap(), right.as_ref().unwrap());
            assert_eq!(l.marker, ' ');
            assert_eq!(r.marker, ' ');
            assert_eq!(l.content, r.content);
        }
    }

    #[test]
    fn diff_side_by_side_detailed_pairs_changed_line_with_word_chunks() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nlet user_id = 42;\nc".into()),
            new: "a\nlet account_id = 42;\nc".into(),
        };
        let rows = diff_side_by_side_detailed(&diff);
        assert_eq!(rows.len(), 3);
        let (l, r) = (rows[1].0.as_ref().unwrap(), rows[1].1.as_ref().unwrap());
        assert_eq!(l.marker, '-');
        assert_eq!(r.marker, '+');
        assert_eq!(
            l.chunks,
            vec![
                DiffChunk::Unchanged("let ".into()),
                DiffChunk::Deleted("user_id".into()),
                DiffChunk::Unchanged(" = 42;".into()),
            ]
        );
        assert_eq!(
            r.chunks,
            vec![
                DiffChunk::Unchanged("let ".into()),
                DiffChunk::Inserted("account_id".into()),
                DiffChunk::Unchanged(" = 42;".into()),
            ]
        );
    }

    #[test]
    fn diff_side_by_side_detailed_pads_unpaired_sides() {
        // New file: every left cell is None.
        let new_file = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "new.txt".into(),
            old: None,
            new: "a\nb".into(),
        };
        let rows = diff_side_by_side_detailed(&new_file);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(l, r)| l.is_none() && r.is_some()));

        // Deletion: the removed line's right cell is None; context below
        // stays aligned.
        let del = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\nb\nc".into()),
            new: "a\nc".into(),
        };
        let rows = diff_side_by_side_detailed(&del);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].1.is_none());
        assert_eq!(rows[1].0.as_ref().unwrap().marker, '-');
        assert_eq!(rows[2].0.as_ref().unwrap().content, "c");
        assert_eq!(rows[2].1.as_ref().unwrap().marker, ' ');
    }

    #[test]
    fn diff_side_by_side_detailed_ending_flip_carries_note() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "a.txt".into(),
            old: Some("a\r\n".into()),
            new: "a\n".into(),
        };
        let rows = diff_side_by_side_detailed(&diff);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.as_ref().unwrap().ending_note, Some("⏎ CRLF → LF"));
        assert_eq!(rows[0].1.as_ref().unwrap().ending_note, Some("⏎ CRLF → LF"));
    }

    #[test]
    fn test_compute_word_diff_identifies_intra_line_changes() {
        let old = "let user_id = 42;";
        let new = "let account_id = 42;";
        let (old_chunks, new_chunks) = compute_word_diff(old, new);

        assert_eq!(
            old_chunks,
            vec![
                DiffChunk::Unchanged("let ".into()),
                DiffChunk::Deleted("user_id".into()),
                DiffChunk::Unchanged(" = 42;".into()),
            ]
        );

        assert_eq!(
            new_chunks,
            vec![
                DiffChunk::Unchanged("let ".into()),
                DiffChunk::Inserted("account_id".into()),
                DiffChunk::Unchanged(" = 42;".into()),
            ]
        );

        // Verification: reconstructing chunks reproduces original lines
        let reconstructed_old: String = old_chunks.iter().map(DiffChunk::text).collect();
        let reconstructed_new: String = new_chunks.iter().map(DiffChunk::text).collect();
        assert_eq!(reconstructed_old, old);
        assert_eq!(reconstructed_new, new);
    }

    #[test]
    fn word_diff_over_budget_falls_back() {
        let old_mid: String = (0..20_000).map(|i| format!("o{i} ")).collect();
        let new_mid: String = (0..20_000).map(|i| format!("n{i} ")).collect();
        let old_line = format!("prefix {old_mid}suffix");
        let new_line = format!("prefix {new_mid}suffix");

        let (old_chunks, new_chunks) = compute_word_diff(&old_line, &new_line);

        assert_eq!(old_chunks.len(), 3);
        assert!(matches!(old_chunks[1], DiffChunk::Deleted(_)));
        assert_eq!(new_chunks.len(), 3);
        assert!(matches!(new_chunks[1], DiffChunk::Inserted(_)));

        let reconstructed_old: String = old_chunks.iter().map(DiffChunk::text).collect();
        let reconstructed_new: String = new_chunks.iter().map(DiffChunk::text).collect();
        assert_eq!(reconstructed_old, old_line);
        assert_eq!(reconstructed_new, new_line);
    }

    #[test]
    fn word_diff_rejoins() {
        let cases = [
            ("let a = 1;", "let a = 1;"),
            ("foo(1, 2)", "foo(1, 3)"),
            ("hello world", "hello there"),
            ("abc", "xyz"),
            ("", "new text"),
            ("old text", ""),
            ("a b c", "a b c d"),
        ];
        for (old, new) in cases {
            let (old_chunks, new_chunks) = compute_word_diff(old, new);
            let reconstructed_old: String = old_chunks.iter().map(DiffChunk::text).collect();
            let reconstructed_new: String = new_chunks.iter().map(DiffChunk::text).collect();
            assert_eq!(
                reconstructed_old, old,
                "old mismatch for {old:?} vs {new:?}"
            );
            assert_eq!(
                reconstructed_new, new,
                "new mismatch for {old:?} vs {new:?}"
            );
        }
    }

    #[test]
    fn test_diff_inline_detailed_with_word_chunks() {
        let diff = FileDiff {
            old_unavailable: false,
            backup_path: None,
            path: "test.rs".into(),
            old: Some("fn foo() -> i32 {\n    return 1;\n}\n".into()),
            new: "fn foo() -> i64 {\n    return 1;\n}\n".into(),
        };
        let detailed = diff_inline_detailed(&diff);
        assert_eq!(detailed.len(), 2);
        assert_eq!(detailed[0].marker, '-');
        assert_eq!(detailed[1].marker, '+');

        assert_eq!(
            detailed[0].chunks,
            vec![
                DiffChunk::Unchanged("fn foo() -> ".into()),
                DiffChunk::Deleted("i32".into()),
                DiffChunk::Unchanged(" {".into()),
            ]
        );
        assert_eq!(
            detailed[1].chunks,
            vec![
                DiffChunk::Unchanged("fn foo() -> ".into()),
                DiffChunk::Inserted("i64".into()),
                DiffChunk::Unchanged(" {".into()),
            ]
        );
    }
    #[test]
    fn tool_stream_chunks_round_trip_with_tags() {
        let chunks = [
            (ToolStreamChunk::Delta("hello".into()), "delta"),
            (
                ToolStreamChunk::Usage(TurnTelemetry {
                    context: None,
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    eval_duration_ms: 5,
                    estimated: false,
                }),
                "usage",
            ),
            (
                ToolStreamChunk::Response(ChatResponse::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }])),
                "response",
            ),
        ];
        for (chunk, kind) in chunks {
            let value = serde_json::to_value(&chunk).unwrap();
            assert_eq!(value["kind"], kind);
            assert!(value.get("value").is_some());
            assert_eq!(
                serde_json::from_value::<ToolStreamChunk>(value).unwrap(),
                chunk
            );
        }
    }

    #[test]
    fn completion_without_preamble_deserializes() {
        let c: ChatCompletion = serde_json::from_str(r#"{"response":{"Text":"hello"}}"#).unwrap();
        assert_eq!(c.preamble, "");
        assert_eq!(c.response, ChatResponse::Text("hello".into()));
    }
    #[test]
    fn literal_reasoning_delimiters_roundtrip() {
        let reasoning = "The literal closing tag is </think>; <think> &lt; then continue.";
        let content = with_reasoning(reasoning, "answer");
        assert_eq!(strip_reasoning(&content), "answer");
        let parsed = tui::parse_thinking(&content);
        assert_eq!(parsed.thinking.as_deref(), Some(reasoning));
        assert_eq!(parsed.answer, "answer");
        assert!(!parsed.is_thinking);
        let partial = format!("{ESCAPED_REASONING_OPEN}{}", escape_reasoning(reasoning));
        let parsed = tui::parse_thinking(&partial);
        assert_eq!(parsed.thinking.as_deref(), Some(reasoning));
        assert!(parsed.is_thinking);
    }

    #[test]
    fn prior_reasoning_is_stripped_only_from_a_complete_leading_block() {
        assert_eq!(strip_reasoning("<think>r</think>answer"), "answer");
        for content in [
            "answer",
            "<think>unfinished",
            "quote <think>r</think>answer",
        ] {
            assert_eq!(strip_reasoning(content), content);
        }
    }
}

pub mod todo;
pub use todo::{TodoItem, TodoPlan, TodoStatus, TodoUpdate};

pub mod tasks;
pub use tasks::{
    AgentTask, NewAgentTask, TaskEvent, TaskHistory, TaskRequest, TaskSnapshot, TaskStatus,
    TaskUpdate,
};

pub mod file_nesting;
pub mod workspace_entries;

pub mod network;
