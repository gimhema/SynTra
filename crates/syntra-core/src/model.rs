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
    #[serde(default)]
    pub review: Review,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Review {
    #[default]
    Pending,
    Approved,
    /// 편집안의 영역 중 일부만 승인된 상태. 영역별 결과는 각 영역의 `review`에 있다.
    PartiallyApproved,
    Rejected {
        reason: String,
    },
}

impl Proposal {
    /// 편집안의 모든 영역이 검토되었으면 제안서 전체의 결과를 정한다.
    pub(crate) fn settle(&mut self) {
        let Change::Edit { regions, .. } = &self.change else {
            return;
        };
        if regions
            .iter()
            .any(|region| region.review == Review::Pending)
        {
            return;
        }
        let reasons: Vec<(usize, &str)> = regions
            .iter()
            .enumerate()
            .filter_map(|(i, region)| match &region.review {
                Review::Rejected { reason } => Some((i + 1, reason.as_str())),
                _ => None,
            })
            .collect();
        self.review = if reasons.is_empty() {
            Review::Approved
        } else if reasons.len() < regions.len() {
            Review::PartiallyApproved
        } else {
            Review::Rejected {
                reason: joined_reason(&reasons),
            }
        };
    }
}

fn joined_reason(reasons: &[(usize, &str)]) -> String {
    let first = reasons[0].1;
    if reasons.iter().all(|(_, reason)| *reason == first) {
        return first.to_string();
    }
    reasons
        .iter()
        .map(|(number, reason)| format!("영역 {number}: {reason}"))
        .collect::<Vec<_>>()
        .join("\n")
}
