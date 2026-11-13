//! The engagement-approval receipt posted to the requester room (TS parity:
//! `lib/engagement-notice.js:2-20` for the content, `lib/matrix-work-executor.js:8-12`
//! for the dispatch). The representative sends one `m.notice` into the request
//! room — a public receipt, never an execution approval or a provider DTO —
//! with the exact retained body wording, an empty `m.mentions`, and an
//! `m.in_reply_to` only when the request's source event id is a real Matrix
//! event id (`$…`).

use serde_json::{Value, json};

/// The notice content (`engagementApprovalContent`,
/// lib/engagement-notice.js:2-20), worded in `language`. `zh` is the retained
/// product's bilingual body byte for byte; `en` and `id` are single-language.
///
/// - `configuration` is `serving.framework · serving.model · serving.reasoning`,
///   joined with ` · ` over the present fields only, and the `Serving:` line is
///   omitted entirely when nothing joined (TS `.filter(Boolean).join(' · ')`).
/// - The closing instruction names the target project room when the request was
///   filed with a request context (TS `engagement.requestContext`), else this
///   room.
/// - `m.mentions` is always the empty allowlist; `m.relates_to` is set only
///   when `source_event_id` starts with `$` (TS line 15-18).
// Not called yet: the request-room approval notice is still unsent natively.
#[allow(dead_code, clippy::too_many_arguments)]
pub fn engagement_approval_content(
    language: hagency_core::notice::NoticeLanguage,
    engagement_id: &str,
    role: &str,
    allocated_tokens: u64,
    project_room_id: &str,
    request_context: bool,
    source_event_id: Option<&str>,
    serving: [Option<&str>; 3],
    mxid: &str,
) -> Value {
    let configuration = serving
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    use hagency_core::notice::NoticeLanguage;
    let (approved, serving_label, request_label) = match language {
        NoticeLanguage::En => (
            format!("Approved {role} for {allocated_tokens} tokens."),
            "Serving",
            "Request",
        ),
        NoticeLanguage::Id => (
            format!("Disetujui {role} untuk {allocated_tokens} token."),
            "Konfigurasi",
            "Permintaan",
        ),
        NoticeLanguage::Zh => (
            format!("已批准 / Approved {role} for {allocated_tokens} tokens."),
            "Serving",
            "Request",
        ),
    };
    let mut body = format!("{approved}\nAgent: {mxid}\n");
    if !configuration.is_empty() {
        body.push_str(&format!("{serving_label}: {configuration}\n"));
    }
    body.push_str(&format!("{request_label}: {engagement_id}\n"));
    body.push_str(&match (language, request_context) {
        (NoticeLanguage::En, true) => format!(
            "Target project: {project_room_id}\nMention this agent in the target project room to begin work."
        ),
        (NoticeLanguage::En, false) => "Mention this agent in this room to begin work.".to_owned(),
        (NoticeLanguage::Id, true) => format!(
            "Project tujuan: {project_room_id}\nSebut agent ini di room project tujuan untuk mulai bekerja."
        ),
        (NoticeLanguage::Id, false) => {
            "Sebut agent ini di room ini untuk mulai bekerja.".to_owned()
        }
        (NoticeLanguage::Zh, true) => format!(
            "目标项目 / Target project: {project_room_id}\n请在目标项目房间 @ 该 Agent 开始任务。Mention this agent in the target project room to begin work."
        ),
        (NoticeLanguage::Zh, false) => {
            "请在此房间 @ 该 Agent 开始任务。Mention this agent in this room to begin work."
                .to_owned()
        }
    });
    let mut content = json!({
        "msgtype": "m.notice",
        "body": body,
        "m.mentions": { "user_ids": [] },
    });
    if let Some(source_event_id) = source_event_id
        && source_event_id.starts_with('$')
    {
        content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": source_event_id } });
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use hagency_core::notice::NoticeLanguage;

    /// The same receipt in English and Indonesian: one language each, the same
    /// lines, no Chinese.
    #[test]
    fn engagement_notice_body_is_worded_in_the_configured_language() {
        let receipt = |language| {
            engagement_approval_content(
                language,
                "en_abc",
                "reviewer",
                250_000,
                "!project:example.test",
                true,
                None,
                [Some("codex"), None, None],
                "@hf_agent:example.test",
            )["body"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            receipt(NoticeLanguage::En),
            "Approved reviewer for 250000 tokens.\n\
             Agent: @hf_agent:example.test\n\
             Serving: codex\n\
             Request: en_abc\n\
             Target project: !project:example.test\n\
             Mention this agent in the target project room to begin work."
        );
        assert_eq!(
            receipt(NoticeLanguage::Id),
            "Disetujui reviewer untuk 250000 token.\n\
             Agent: @hf_agent:example.test\n\
             Konfigurasi: codex\n\
             Permintaan: en_abc\n\
             Project tujuan: !project:example.test\n\
             Sebut agent ini di room project tujuan untuk mulai bekerja."
        );
        for language in [NoticeLanguage::En, NoticeLanguage::Id] {
            assert!(
                !receipt(language)
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{language:?}"
            );
        }
    }

    /// TS parity, lib/engagement-notice.js:5-13: the body lines for a request
    /// with context and a full serving configuration.
    #[test]
    fn engagement_notice_body_with_context_and_serving() {
        let content = engagement_approval_content(
            hagency_core::notice::NoticeLanguage::Zh,
            "en_abc",
            "reviewer",
            250_000,
            "!project:example.test",
            true,
            Some("$requestevent"),
            [Some("codex"), Some("gpt-5"), Some("high")],
            "@hf_agent:example.test",
        );
        assert_eq!(content["msgtype"], "m.notice");
        assert_eq!(
            content["body"].as_str().unwrap(),
            "已批准 / Approved reviewer for 250000 tokens.\n\
             Agent: @hf_agent:example.test\n\
             Serving: codex · gpt-5 · high\n\
             Request: en_abc\n\
             目标项目 / Target project: !project:example.test\n\
             请在目标项目房间 @ 该 Agent 开始任务。Mention this agent in the target project room to begin work."
        );
        assert_eq!(content["m.mentions"], json!({ "user_ids": [] }));
        // TS lines 15-18: a `$`-prefixed source event threads the receipt.
        assert_eq!(
            content["m.relates_to"],
            json!({ "m.in_reply_to": { "event_id": "$requestevent" } })
        );
    }

    /// TS parity: partial serving fields join with ` · `; without a request
    /// context the closing instruction names this room; a non-`$` request id
    /// adds no relation.
    #[test]
    fn engagement_notice_body_without_context_and_partial_serving() {
        let content = engagement_approval_content(
            hagency_core::notice::NoticeLanguage::Zh,
            "en_def",
            "builder",
            1000,
            "!project:example.test",
            false,
            Some("req-not-an-event"),
            [Some("claude"), None, None],
            "@hf_other:example.test",
        );
        assert_eq!(
            content["body"].as_str().unwrap(),
            "已批准 / Approved builder for 1000 tokens.\n\
             Agent: @hf_other:example.test\n\
             Serving: claude\n\
             Request: en_def\n\
             请在此房间 @ 该 Agent 开始任务。Mention this agent in this room to begin work."
        );
        assert!(content.get("m.relates_to").is_none());
    }

    /// TS parity, line 9: an empty configuration omits the Serving line whole.
    #[test]
    fn engagement_notice_body_without_serving() {
        let content = engagement_approval_content(
            hagency_core::notice::NoticeLanguage::Zh,
            "en_ghi",
            "reviewer",
            5,
            "!p:example.test",
            false,
            None,
            [None, None, None],
            "@a:b",
        );
        let body = content["body"].as_str().unwrap();
        assert!(!body.contains("Serving:"));
        assert_eq!(
            body,
            "已批准 / Approved reviewer for 5 tokens.\nAgent: @a:b\nRequest: en_ghi\n请在此房间 @ 该 Agent 开始任务。Mention this agent in this room to begin work."
        );
        assert!(content.get("m.relates_to").is_none());
    }
}
