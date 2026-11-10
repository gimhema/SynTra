use serde::{Deserialize, Serialize};

/// 에이전트가 작성하는 제안서 한 건. 디스크에는 JSON으로 저장된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// 제안서가 속한 작업. 없으면 id에서 끝의 `_번호`를 뗀 값을 쓴다. (`Proposal::task_of`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(flatten)]
    pub change: Change,
    /// 생성안의 내용을 사용자가 검토 화면에서 직접 고쳤는지. 편집안은 영역마다 기록한다.
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
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
    /// 편집 후 코드를 사용자가 검토 화면에서 직접 고쳤는지.
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    #[serde(default)]
    pub review: Review,
}

fn is_false(value: &bool) -> bool {
    !value
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
    /// 제안서가 속한 작업 이름. `task`가 없으면 id 규칙(`<날짜>_<시각>_<작업명>_<번호>`)에서
    /// 끝의 `_번호`를 떼어 정한다. 번호가 없으면 id 자체가 작업이 된다.
    pub fn task_of(&self, id: &str) -> String {
        if let Some(task) = self
            .task
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return task.to_string();
        }
        match id.rsplit_once('_') {
            Some((task, number))
                if !task.is_empty()
                    && !number.is_empty()
                    && number.chars().all(|c| c.is_ascii_digit()) =>
            {
                task.to_string()
            }
            _ => id.to_string(),
        }
    }

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
