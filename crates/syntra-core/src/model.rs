use serde::{Deserialize, Serialize};

/// 에이전트가 작성하는 제안서 한 건. 디스크에는 JSON으로 저장된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(flatten)]
    pub change: Change,
    #[serde(default)]
    pub review: Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Create {
        file_name: String,
        content: String,
        purpose: String,
        location: String,
    },
    Edit {
        file_name: String,
        regions: Vec<EditRegion>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRegion {
    pub before: String,
    pub after: String,
    pub purpose: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Review {
    #[default]
    Pending,
    Approved,
    Rejected {
        reason: String,
    },
}
