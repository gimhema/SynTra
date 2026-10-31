use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::apply::apply;
use crate::model::{Proposal, Review};

const PROPOSALS_DIR: &str = ".syntra/proposals";
const FALLBACK_PROPOSALS_DIR: &str = "proposals";

/// 워크스페이스의 제안서 디렉터리. 제안서 하나가 `<id>.json` 파일 하나에 대응한다.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
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
    /// 제안서를 찾은 디렉터리. 목록이 비었을 때 사용자에게 알려주기 위해 담는다.
    pub source: String,
    pub proposals: Vec<Entry>,
    pub invalid: Vec<InvalidEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("거부 사유를 입력해야 합니다")]
    BlankReason,
    #[error("제안서를 찾을 수 없습니다: {0}")]
    NotFound(String),
    #[error("이미 처리된 제안서입니다: {0}")]
    AlreadyReviewed(String),
    #[error("적용할 수 없습니다. {0}")]
    Apply(String),
    #[error("허용되지 않는 경로입니다: {0}")]
    UnsafePath(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("제안서 파일을 읽을 수 없습니다: {0}")]
    Json(#[from] serde_json::Error),
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            write_lock: Mutex::new(()),
        }
    }

    /// 제안서 폴더 자체를 고른 경우에도 워크스페이스 최상위를 돌려준다.
    pub fn workspace_root(path: &Path) -> PathBuf {
        let path: PathBuf = path.components().collect();
        if path
            .file_name()
            .is_none_or(|name| name != FALLBACK_PROPOSALS_DIR)
        {
            return path;
        }
        let Some(parent) = path.parent() else {
            return path;
        };
        if parent.file_name().is_some_and(|name| name == ".syntra") {
            return parent.parent().unwrap_or(parent).to_path_buf();
        }
        if has_json_file(&path) {
            return parent.to_path_buf();
        }
        path
    }

    fn dir(&self) -> PathBuf {
        let primary = self.root.join(PROPOSALS_DIR);
        let fallback = self.root.join(FALLBACK_PROPOSALS_DIR);
        if !primary.is_dir() && fallback.is_dir() {
            fallback
        } else {
            primary
        }
    }

    pub fn list(&self) -> io::Result<Listing> {
        let dir = self.dir();
        let mut listing = Listing {
            source: dir.display().to_string(),
            ..Listing::default()
        };
        let entries = match fs::read_dir(&dir) {
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

    /// 제안서를 워크스페이스에 적용하고 승인 상태로 기록한다. 적용에 실패하면 대기 상태로 남는다.
    pub fn approve(&self, id: &str) -> Result<Proposal, ReviewError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        apply(&self.root, &self.dir(), &proposal.change)?;
        proposal.review = Review::Approved;
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    /// 제안서를 거부 상태로 기록한다. 제안서 파일 외에는 아무것도 변경하지 않는다.
    pub fn reject(&self, id: &str, reason: &str) -> Result<Proposal, ReviewError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(ReviewError::BlankReason);
        }

        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        proposal.review = Review::Rejected {
            reason: reason.to_string(),
        };
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    fn load_pending(&self, id: &str) -> Result<Proposal, ReviewError> {
        // id는 URL에서 오므로 디렉터리 밖을 가리키지 못하게 막는다.
        if !is_valid_id(id) {
            return Err(ReviewError::NotFound(id.to_string()));
        }
        let path = self.dir().join(format!("{id}.json"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(ReviewError::NotFound(id.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        let proposal: Proposal = serde_json::from_str(&text)?;
        if proposal.review != Review::Pending {
            return Err(ReviewError::AlreadyReviewed(id.to_string()));
        }
        Ok(proposal)
    }

    fn save(&self, id: &str, proposal: &Proposal) -> Result<(), ReviewError> {
        // 쓰기 도중 실패해도 원본이 깨지지 않도록 임시 파일을 거쳐 교체한다.
        let dir = self.dir();
        let tmp = dir.join(format!("{id}.json.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(proposal)?)?;
        fs::rename(&tmp, dir.join(format!("{id}.json")))?;
        Ok(())
    }
}

fn has_json_file(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
    })
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
    use std::process::Command;

    const CREATE: &str = r#"{"kind":"create","file_name":"a.rs","content":"fn a() {}","purpose":"p","location":"src/"}"#;

    fn proposal_path(root: &Path, id: &str) -> PathBuf {
        root.join(PROPOSALS_DIR).join(format!("{id}.json"))
    }

    fn write_proposal(root: &Path, id: &str, json: &str) {
        fs::create_dir_all(root.join(PROPOSALS_DIR)).unwrap();
        fs::write(proposal_path(root, id), json).unwrap();
    }

    fn store_with(id: &str, json: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        write_proposal(dir.path(), id, json);
        let store = Store::new(dir.path());
        (dir, store)
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// `src/main.rs`가 있는 워크스페이스를 만든다.
    fn repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "one\ntwo\ntwo\n").unwrap();
        fs::write(root.join("other.txt"), "x\n").unwrap();
        let store = Store::new(root);
        (dir, store)
    }

    fn edit_json(before: &str, after: &str) -> String {
        serde_json::json!({
            "kind": "edit",
            "file_name": "src/main.rs",
            "regions": [{"before": before, "after": after, "purpose": "목적"}],
        })
        .to_string()
    }

    fn create_json(location: &str, file_name: &str) -> String {
        serde_json::json!({
            "kind": "create",
            "file_name": file_name,
            "content": "new\n",
            "purpose": "p",
            "location": location,
        })
        .to_string()
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
                Err(ReviewError::BlankReason)
            ));
        }
        assert_eq!(
            fs::read_to_string(proposal_path(dir.path(), "one")).unwrap(),
            CREATE
        );
    }

    #[test]
    fn reject_does_not_apply_the_change() {
        let (dir, store) = store_with("one", CREATE);
        store.reject("one", "사유").unwrap();
        assert!(!dir.path().join("src").exists());
    }

    #[test]
    fn second_reject_keeps_first_reason() {
        let (_dir, store) = store_with("one", CREATE);
        store.reject("one", "첫 사유").unwrap();
        assert!(matches!(
            store.reject("one", "둘째 사유"),
            Err(ReviewError::AlreadyReviewed(_))
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
            Err(ReviewError::NotFound(_))
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
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let listing = Store::new(dir.path().join("none")).list().unwrap();
        assert!(listing.proposals.is_empty());
    }

    #[test]
    fn approve_create_places_file() {
        let (dir, store) = repo();
        write_proposal(dir.path(), "c", &create_json("src/new/", "a.rs"));
        let proposal = store.approve("c").unwrap();
        assert_eq!(proposal.review, Review::Approved);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/new/a.rs")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn approve_create_never_overwrites() {
        let (dir, store) = repo();
        write_proposal(dir.path(), "c", &create_json("", "other.txt"));
        assert!(matches!(store.approve("c"), Err(ReviewError::Apply(_))));
        assert_eq!(
            fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "x\n"
        );
        assert_eq!(
            store.list().unwrap().proposals[0].proposal.review,
            Review::Pending
        );
    }

    #[test]
    fn unsafe_paths_are_refused() {
        let (dir, store) = repo();
        let outside = dir.path().join("../syntra-escape.txt");
        let cases = [
            ("../", "syntra-escape.txt"),
            ("src/../../", "syntra-escape.txt"),
            (".git/hooks/", "pre-commit"),
            (".GIT/hooks/", "pre-commit"),
            (".syntra/proposals/", "x.json"),
            ("", ""),
        ];
        for (location, file_name) in cases {
            write_proposal(dir.path(), "c", &create_json(location, file_name));
            assert!(
                matches!(store.approve("c"), Err(ReviewError::UnsafePath(_))),
                "{location}{file_name}"
            );
        }
        let absolute = outside.display().to_string();
        write_proposal(dir.path(), "c", &create_json(&absolute, "a"));
        assert!(matches!(
            store.approve("c"),
            Err(ReviewError::UnsafePath(_))
        ));
        assert!(!outside.exists());
        assert!(!dir.path().join(".git/hooks/pre-commit").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_refused() {
        let (dir, store) = repo();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        write_proposal(dir.path(), "c", &create_json("link/", "a.rs"));
        assert!(matches!(
            store.approve("c"),
            Err(ReviewError::UnsafePath(_))
        ));
        assert!(!outside.path().join("a.rs").exists());
    }

    #[test]
    fn approve_edit_replaces_only_the_region() {
        let (dir, store) = repo();
        let root = dir.path();
        write_proposal(root, "e", &edit_json("one\n", "ONE\n"));

        let proposal = store.approve("e").unwrap();

        assert_eq!(proposal.review, Review::Approved);
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "ONE\ntwo\ntwo\n"
        );
        assert_eq!(fs::read_to_string(root.join("other.txt")).unwrap(), "x\n");
    }

    #[test]
    fn approve_keeps_uncommitted_changes_and_git_history() {
        let (dir, store) = repo();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["add", "src/main.rs"]);
        git(root, &["commit", "-qm", "init"]);
        fs::write(root.join("src/main.rs"), "one\ntwo\ntwo\nlocal\n").unwrap();
        write_proposal(root, "e", &edit_json("one\n", "ONE\n"));
        write_proposal(root, "c", &create_json("src/", "new.rs"));

        store.approve("e").unwrap();
        store.approve("c").unwrap();

        assert_eq!(git(root, &["rev-list", "--count", "HEAD"]).trim(), "1");
        assert_eq!(git(root, &["diff", "--cached", "--name-only"]).trim(), "");
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "ONE\ntwo\ntwo\nlocal\n"
        );
    }

    #[test]
    fn approve_edit_refuses_missing_or_ambiguous_before() {
        for before in ["nope", "two\n", ""] {
            let (dir, store) = repo();
            write_proposal(dir.path(), "e", &edit_json(before, "x"));
            assert!(
                matches!(store.approve("e"), Err(ReviewError::Apply(_))),
                "{before}"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
                "one\ntwo\ntwo\n"
            );
        }
    }

    #[test]
    fn plain_proposals_directory_is_used_as_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let proposals = dir.path().join("proposals");
        fs::create_dir(&proposals).unwrap();
        fs::write(proposals.join("one.json"), CREATE).unwrap();
        let store = Store::new(dir.path());

        let listing = store.list().unwrap();
        assert_eq!(listing.proposals[0].id, "one");
        assert_eq!(listing.source, proposals.display().to_string());

        store.approve("one").unwrap();
        assert!(dir.path().join("src/a.rs").exists());
        assert!(
            fs::read_to_string(proposals.join("one.json"))
                .unwrap()
                .contains("approved")
        );
    }

    #[test]
    fn syntra_directory_takes_priority_over_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_proposal(dir.path(), "main", CREATE);
        fs::create_dir(dir.path().join("proposals")).unwrap();
        fs::write(dir.path().join("proposals/plain.json"), CREATE).unwrap();
        let listing = Store::new(dir.path()).list().unwrap();
        assert_eq!(listing.proposals.len(), 1);
        assert_eq!(listing.proposals[0].id, "main");
    }

    #[test]
    fn proposal_cannot_write_into_proposals_directory() {
        let dir = tempfile::tempdir().unwrap();
        let proposals = dir.path().join("proposals");
        fs::create_dir(&proposals).unwrap();
        fs::write(
            proposals.join("one.json"),
            create_json("proposals/", "x.json"),
        )
        .unwrap();
        assert!(matches!(
            Store::new(dir.path()).approve("one"),
            Err(ReviewError::UnsafePath(_))
        ));
        assert!(!proposals.join("x.json").exists());
    }

    #[test]
    fn workspace_root_understands_proposals_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let plain = root.join("plain/proposals");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("one.json"), CREATE).unwrap();
        let source = root.join("code/proposals");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("mod.rs"), "").unwrap();

        assert_eq!(Store::workspace_root(&plain), root.join("plain"));
        assert_eq!(
            Store::workspace_root(&root.join("w/.syntra/proposals")),
            root.join("w")
        );
        assert_eq!(Store::workspace_root(&source), source);
        assert_eq!(Store::workspace_root(&root.join("x/")), root.join("x"));
    }

    #[test]
    fn previously_recorded_commit_field_is_ignored() {
        let proposal: Proposal = serde_json::from_str(
            r#"{"kind":"create","file_name":"a","content":"","purpose":"","location":"",
                "review":{"status":"approved","commit":"abc"}}"#,
        )
        .unwrap();
        assert_eq!(proposal.review, Review::Approved);
    }

    #[test]
    fn approved_proposal_cannot_be_reviewed_again() {
        let (dir, store) = repo();
        write_proposal(dir.path(), "c", &create_json("", "a.rs"));
        store.approve("c").unwrap();
        assert!(matches!(
            store.approve("c"),
            Err(ReviewError::AlreadyReviewed(_))
        ));
        assert!(matches!(
            store.reject("c", "r"),
            Err(ReviewError::AlreadyReviewed(_))
        ));
    }

    #[test]
    fn bundled_examples_are_valid() {
        for json in [
            include_str!("../../../examples/proposals/create-greeting.json"),
            include_str!("../../../examples/proposals/edit-main.json"),
        ] {
            serde_json::from_str::<Proposal>(json).unwrap();
        }
    }
}
