//! Vocabulary shared by storage and the memory lifecycle.

/// How deeply a memory is burned in. Ordered from weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Faint,
    Normal,
    Vivid,
    Seared,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Faint, Level::Normal, Level::Vivid, Level::Seared];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Faint => "faint",
            Level::Normal => "normal",
            Level::Vivid => "vivid",
            Level::Seared => "seared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// One step up for an escalation she didn't choose (an unresolved thread, a
    /// broken promise). Escalation stops at `Vivid`; only her own judgment when
    /// writing, or you from the CLI, can sear a memory.
    pub fn escalated(self) -> Self {
        match self {
            Level::Faint => Level::Normal,
            Level::Normal | Level::Vivid => Level::Vivid,
            Level::Seared => Level::Seared,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    UserFact,
    Preference,
    Episode,
    Relationship,
    Promise,
    Opinion,
    OpenThread,
    PersonaNote,
}

impl MemoryType {
    pub const ALL: [MemoryType; 8] = [
        MemoryType::UserFact,
        MemoryType::Preference,
        MemoryType::Episode,
        MemoryType::Relationship,
        MemoryType::Promise,
        MemoryType::Opinion,
        MemoryType::OpenThread,
        MemoryType::PersonaNote,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::UserFact => "user_fact",
            MemoryType::Preference => "preference",
            MemoryType::Episode => "episode",
            MemoryType::Relationship => "relationship",
            MemoryType::Promise => "promise",
            MemoryType::Opinion => "opinion",
            MemoryType::OpenThread => "open_thread",
            MemoryType::PersonaNote => "persona_note",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}
