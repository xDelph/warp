use std::collections::HashMap;

use warp_cli::agent::Harness;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::conversation::AIConversationId;

pub(crate) struct LocalAcpSessionStore {
    sessions_by_harness: HashMap<Harness, String>,
    /// The harness that most recently completed a prompt for each
    /// conversation. Used to detect harness switches mid-conversation so the
    /// new harness's session can be primed with the prior transcript. Only
    /// recorded on successful completion, which matches the ACP worker session
    /// lifetime (failed sessions are torn down and restarted context-free).
    last_harness_by_conversation: HashMap<AIConversationId, Harness>,
}

impl LocalAcpSessionStore {
    pub(crate) fn new(_: &mut ModelContext<Self>) -> Self {
        Self {
            sessions_by_harness: HashMap::new(),
            last_harness_by_conversation: HashMap::new(),
        }
    }

    pub(crate) fn session_id(&self, harness: Harness) -> Option<&str> {
        self.sessions_by_harness
            .get(&harness)
            .map(std::string::String::as_str)
    }

    pub(crate) fn set_session_id(&mut self, harness: Harness, session_id: String) {
        self.sessions_by_harness.insert(harness, session_id);
    }

    pub(crate) fn last_harness(&self, conversation_id: AIConversationId) -> Option<Harness> {
        self.last_harness_by_conversation
            .get(&conversation_id)
            .copied()
    }

    pub(crate) fn set_last_harness(
        &mut self,
        conversation_id: AIConversationId,
        harness: Harness,
    ) {
        self.last_harness_by_conversation
            .insert(conversation_id, harness);
    }
}

impl Entity for LocalAcpSessionStore {
    type Event = ();
}

impl SingletonEntity for LocalAcpSessionStore {}
