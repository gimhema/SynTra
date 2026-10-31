use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::model::{Proposal, Review};

/// 제안서 디렉터리. 제안서 하나가 `<id>.json` 파일 하나에 대응한다.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    write_lock: Mutex<()>,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: String,
    #[serde(flatten)]
    pub proposal: Proposal,
}

/// 읽지 못한 파일도 숨기지 않고 사용자에게 보여주기 위해 따로 담는다.
#[derive(Debug, Serialize)]
pub struct InvalidEntry {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Listing {
    pub proposals: Vec<Entry>,
    pub invalid: Vec<InvalidEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum RejectError {
    #[error("거부 사유를 입력해야 합니다")]
    BlankReason,
    #[error("제안서를 찾을 수 없습니다: {0}")]
    NotFound(String),
    #[error("이미 처리된 제안서입니다: {0}")]
    AlreadyReviewed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("제안서 파일을 읽을 수 없습니다: {0}")]
    Json(#[from] serde_json::Error),
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> io::Result<Listing> {
        let mut listing = Listing::default();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(e),
        };

        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match read_entry(&path) {
                Ok(entry) => listing.proposals.push(entry),
                Err(error) => listing.invalid.push(InvalidEntry {
                    file: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    error,
                }),
            }
        }
        listing.proposals.sort_by(|a, b| a.id.cmp(&b.id));
        listing.invalid.sort_by(|a, b| a.file.cmp(&b.file));
        Ok(listing)
    }

    /// 제안서를 거부 상태로 기록한다. 제안서 파일 외에는 아무것도 변경하지 않는다.
    pub fn reject(&self, id: &str, reason: &str) -> Result<Proposal, RejectError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(RejectError::BlankReason);
        }
        // id는 URL에서 오므로 디렉터리 밖을 가리키지 못하게 막는다.
        if !is_valid_id(id) {
            return Err(RejectError::NotFound(id.to_string()));
        }

        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.dir.join(format!("{id}.json"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(RejectError::NotFound(id.to_string()));
            }
            Err(e) => return Err(e.into()),
        };

        let mut proposal: Proposal = serde_json::from_str(&text)?;
        if proposal.review != Review::Pending {
            return Err(RejectError::AlreadyReviewed(id.to_string()));
        }
        proposal.review = Review::Rejected {
            reason: reason.to_string(),
        };

        // 쓰기 도중 실패해도 원본이 깨지지 않도록 임시 파일을 거쳐 교체한다.
        let tmp = self.dir.join(format!("{id}.json.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&proposal)?)?;
        fs::rename(&tmp, &path)?;
        Ok(proposal)
    }
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn read_entry(path: &Path) -> Result<Entry, String> {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| is_valid_id(stem))
        .ok_or("파일명은 영문, 숫자, '-', '_'만 사용할 수 있습니다")?
        .to_string();
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let proposal = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Entry { id, proposal })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Change;

    const CREATE: &str = r#"{"kind":"create","file_name":"a.rs","content":"fn a() {}","purpose":"p","location":"src/"}"#;

    fn store_with(id: &str, json: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(format!("{id}.json")), json).unwrap();
        let store = Store::new(dir.path());
        (dir, store)
    }

    #[test]
    fn missing_review_means_pending() {
        let (_dir, store) = store_with("one", CREATE);
        let listing = store.list().unwrap();
        assert_eq!(listing.proposals[0].proposal.review, Review::Pending);
        assert!(matches!(
            listing.proposals[0].proposal.change,
            Change::Create { .. }
        ));
    }

    #[test]
    fn reject_persists_trimmed_reason() {
        let (_dir, store) = store_with("one", CREATE);
        store.reject("one", "  범위 초과  ").unwrap();
        assert_eq!(
            store.list().unwrap().proposals[0].proposal.review,
            Review::Rejected {
                reason: "범위 초과".to_string()
            }
        );
    }

    #[test]
    fn blank_reason_is_refused_and_file_is_untouched() {
        let (dir, store) = store_with("one", CREATE);
        for reason in ["", "  \n\t "] {
            assert!(matches!(
                store.reject("one", reason),
                Err(RejectError::BlankReason)
            ));
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("one.json")).unwrap(),
            CREATE
        );
    }

    #[test]
    fn reject_only_touches_the_proposal_file() {
        let (dir, store) = store_with("one", CREATE);
        store.reject("one", "사유").unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["one.json"]);
    }

    #[test]
    fn second_reject_keeps_first_reason() {
        let (_dir, store) = store_with("one", CREATE);
        store.reject("one", "첫 사유").unwrap();
        assert!(matches!(
            store.reject("one", "둘째 사유"),
            Err(RejectError::AlreadyReviewed(_))
        ));
        assert_eq!(
            store.list().unwrap().proposals[0].proposal.review,
            Review::Rejected {
                reason: "첫 사유".to_string()
            }
        );
    }

    #[test]
    fn path_traversal_id_is_not_found() {
        let (_dir, store) = store_with("one", CREATE);
        assert!(matches!(
            store.reject("../one", "사유"),
            Err(RejectError::NotFound(_))
        ));
    }

    #[test]
    fn broken_file_is_listed_as_invalid() {
        let (_dir, store) = store_with("broken", "{");
        let listing = store.list().unwrap();
        assert!(listing.proposals.is_empty());
        assert_eq!(listing.invalid[0].file, "broken.json");
    }

    #[test]
    fn bundled_examples_are_valid() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/proposals");
        let listing = Store::new(dir).list().unwrap();
        assert_eq!(listing.proposals.len(), 2);
        assert!(listing.invalid.is_empty());
    }

    #[test]
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let listing = Store::new(dir.path().join("none")).list().unwrap();
        assert!(listing.proposals.is_empty());
    }
}
