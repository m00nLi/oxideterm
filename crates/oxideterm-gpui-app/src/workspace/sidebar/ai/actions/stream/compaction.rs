// GPUI notice lifecycle remains app-owned because it schedules entity updates and repaints.
impl WorkspaceApp {
    pub(in crate::workspace) fn set_ai_compaction_notice_running(
        &mut self,
        conversation_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.ai.chat.compaction_notice = Some(AiCompactionNotice {
            conversation_id: conversation_id.to_string(),
            phase: AiCompactionNoticePhase::Running,
            compacted_count: None,
            timestamp_ms: ai_now_ms(),
        });
        cx.notify();
    }

    pub(in crate::workspace) fn set_ai_compaction_notice_done(
        &mut self,
        conversation_id: &str,
        compacted_count: usize,
        cx: &mut Context<Self>,
    ) {
        let timestamp_ms = ai_now_ms();
        self.ai.chat.compaction_notice = Some(AiCompactionNotice {
            conversation_id: conversation_id.to_string(),
            phase: AiCompactionNoticePhase::Done,
            compacted_count: Some(compacted_count),
            timestamp_ms,
        });
        self.schedule_ai_compaction_notice_clear(conversation_id.to_string(), timestamp_ms, cx);
        cx.notify();
    }

    pub(in crate::workspace) fn clear_ai_compaction_notice_for(
        &mut self,
        conversation_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self
            .ai
            .chat
            .compaction_notice
            .as_ref()
            .is_some_and(|notice| notice.conversation_id == conversation_id)
        {
            self.ai.chat.compaction_notice = None;
            cx.notify();
        }
    }

    pub(in crate::workspace) fn schedule_ai_compaction_notice_clear(
        &mut self,
        conversation_id: String,
        timestamp_ms: i64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |weak, cx| {
            Timer::after(Duration::from_secs(5)).await;
            let _ = weak.update(cx, |this, cx| {
                if this
                    .ai
                    .chat
                    .compaction_notice
                    .as_ref()
                    .is_some_and(|notice| {
                        notice.conversation_id == conversation_id
                            && notice.phase == AiCompactionNoticePhase::Done
                            && notice.timestamp_ms == timestamp_ms
                    })
                {
                    this.ai.chat.compaction_notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
