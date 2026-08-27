//! Durable, harness-neutral launch specifications.

use serde::{Deserialize, Serialize};

/// Harness-neutral, fully materialized execution inputs. Values are persisted
/// at enqueue time so later card/column/config edits cannot change a launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub agent_kind: Option<String>,
    pub initial_prompt: Option<String>,
    pub system_prompt: Option<String>,
}

/// Durable launch description with an independent format version. Placement
/// remains daemon-owned; unsupported versions are rejected rather than guessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunLaunchSpec {
    version: u32,
    execution: ExecutionSpec,
}

impl RunLaunchSpec {
    pub const VERSION: u32 = 1;
    pub const EXTERNAL_VERSION: u32 = 2;

    pub fn v1(execution: ExecutionSpec) -> Self {
        Self {
            version: Self::VERSION,
            execution,
        }
    }

    /// A live process observed and linked by the board, but not started or
    /// owned by it. External runs are watched, never killed or rescued.
    pub fn external_v2(execution: ExecutionSpec) -> Self {
        Self {
            version: Self::EXTERNAL_VERSION,
            execution,
        }
    }

    pub fn is_external(&self) -> bool {
        self.version == Self::EXTERNAL_VERSION
    }

    pub fn execution(&self) -> &ExecutionSpec {
        &self.execution
    }
}

impl<'de> Deserialize<'de> for RunLaunchSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DurableSpec {
            version: u32,
            execution: ExecutionSpec,
        }

        let spec = DurableSpec::deserialize(deserializer)?;
        if !matches!(spec.version, Self::VERSION | Self::EXTERNAL_VERSION) {
            return Err(serde::de::Error::custom(format!(
                "unsupported launch spec version {}",
                spec.version
            )));
        }
        Ok(Self {
            version: spec.version,
            execution: spec.execution,
        })
    }
}
