//! Vocabulary shared by storage and the memory lifecycle. Each enum is stored as
//! the text the schema's CHECK constraints expect.

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                let s = value.as_str()?;
                Self::parse(s).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!("unknown {} {s:?}", stringify!($name)).into(),
                    )
                })
            }
        }
    };
}

text_enum! {
    /// How deeply a memory is burned in. Ordered from weakest to strongest.
    Level { Faint = "faint", Normal = "normal", Vivid = "vivid", Seared = "seared" }
}

impl Level {
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

text_enum! {
    MemoryType {
        UserFact = "user_fact",
        Preference = "preference",
        Episode = "episode",
        Relationship = "relationship",
        Promise = "promise",
        Opinion = "opinion",
        OpenThread = "open_thread",
        PersonaNote = "persona_note",
    }
}

text_enum! {
    /// How urgently an open thread wants resolving.
    Urgency { Simmering = "simmering", Hot = "hot" }
}

text_enum! {
    Status { Active = "active", Superseded = "superseded" }
}

text_enum! {
    /// Who said a turn.
    Speaker { User = "user", Companion = "companion" }
}

text_enum! {
    /// How a session ended.
    SessionEnd { Sleep = "sleep", Doctor = "doctor" }
}

text_enum! {
    /// Who made a write. The companion (through the writer) can only raise
    /// levels; the CLI can set any level except on a seared memory.
    Actor { Writer = "writer", Cli = "cli" }
}

text_enum! {
    /// The soul.md sections she may edit. The persona itself is a file she never writes.
    Section { AboutYou = "about_you", Relationship = "relationship" }
}
