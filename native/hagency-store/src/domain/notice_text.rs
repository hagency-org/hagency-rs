//! The fixed words of the progress notices Hagency posts into a task thread,
//! per [`NoticeLanguage`]. A notice's kind, routing and timing are decided by
//! the state machine that queues it; this module only words it. English is
//! the retained product's text verbatim. The activity notice words live with
//! their state machine in `activity.rs`.
use hagency_core::notice::NoticeLanguage;

/// The over-budget notice: fixed words around the elapsed time, never free
/// text. It names the stop that exists in this port, the operator's stop of
/// the agent from the console.
pub fn over_budget_notice_body(language: NoticeLanguage, elapsed_ms: u64) -> String {
    let minutes = elapsed_ms >= 60_000;
    let (amount, unit) = if minutes {
        (elapsed_ms / 60_000, ["min", "menit", "分钟"])
    } else {
        (elapsed_ms / 1_000, ["s", "detik", "秒"])
    };
    match language {
        NoticeLanguage::En => format!(
            "Still running after {amount} {}. It continues until the runner finishes its turn; an operator can stop the agent from the console.",
            unit[0]
        ),
        NoticeLanguage::Id => format!(
            "Masih berjalan setelah {amount} {}. Proses berlanjut sampai runner menyelesaikan gilirannya; operator bisa menghentikan agent dari console.",
            unit[1]
        ),
        NoticeLanguage::Zh => format!(
            "已运行 {amount} {}，仍在进行。它会持续到运行器完成本轮；操作员可在控制台停止该 Agent。",
            unit[2]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::over_budget_notice_body;
    use hagency_core::notice::NoticeLanguage;

    #[test]
    fn over_budget_notice_is_worded_per_language() {
        assert_eq!(
            over_budget_notice_body(NoticeLanguage::En, 125_000),
            "Still running after 2 min. It continues until the runner finishes its turn; an operator can stop the agent from the console."
        );
        assert!(
            over_budget_notice_body(NoticeLanguage::Id, 45_500)
                .starts_with("Masih berjalan setelah 45 detik.")
        );
        assert!(over_budget_notice_body(NoticeLanguage::Zh, 125_000).starts_with("已运行 2 分钟"));
    }
}
