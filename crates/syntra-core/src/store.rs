use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::apply::{PendingWrite, plan_create, plan_edit, read_text};
use crate::model::{Change, EditRegion, Proposal, Review};
use crate::text::Config;

const PROPOSALS_DIR: &str = ".syntra/proposals";
const FALLBACK_PROPOSALS_DIR: &str = "proposals";
const CONFIG_FILE: &str = ".syntra/config.json";

/// 워크스페이스의 제안서 디렉터리. 제안서 하나가 `<id>.json` 파일 하나에 대응한다.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    write_lock: Mutex<()>,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: String,
    /// 같은 작업의 제안서를 묶어 보여주기 위한 작업 이름. (`Proposal::task_of`)
    pub task: String,
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

/// 수동 편집 결과. 고친 내용은 저장하되, 지금 승인하면 실패할 이유가 있으면 `problem`에 담는다.
#[derive(Debug, Serialize)]
pub struct Edited {
    pub proposal: Proposal,
    pub problem: Option<String>,
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
    #[error("설정 파일({CONFIG_FILE})을 읽을 수 없습니다: {0}")]
    Config(String),
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

    /// 승인할 때마다 읽으므로 설정을 바꾸면 서버를 다시 띄우지 않아도 반영된다.
    pub fn config(&self) -> Result<Config, ReviewError> {
        let text = match fs::read_to_string(self.root.join(CONFIG_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(ReviewError::Config(e.to_string())),
        };
        serde_json::from_str(&text).map_err(|e| ReviewError::Config(e.to_string()))
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

    /// 제안서를 워크스페이스에 적용하고 승인 상태로 기록한다. 편집안은 아직 대기 중인 영역을 모두
    /// 적용한다. 적용에 실패하면 아무것도 바뀌지 않고 대기 상태로 남는다.
    pub fn approve(&self, id: &str) -> Result<Proposal, ReviewError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        self.plan(&proposal)?.commit()?;
        match &mut proposal.change {
            Change::Create { .. } => proposal.review = Review::Approved,
            Change::Edit { regions, .. } => {
                for region in regions.iter_mut() {
                    if region.review == Review::Pending {
                        region.review = Review::Approved;
                    }
                }
                proposal.settle();
            }
        }
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    /// 승인했을 때 적용될 수 있는지만 확인한다. 워크스페이스와 제안서 파일은 바꾸지 않는다.
    /// 에이전트가 제안서를 넘기기 전에 스스로 검증하는 데 쓴다.
    pub fn check(&self, id: &str) -> Result<(), ReviewError> {
        self.plan(&self.load_pending(id)?).map(drop)
    }

    /// 워크스페이스 파일을 승인 시 비교하는 그대로 읽는다. (형식 설명, 내용)을 돌려준다.
    pub fn read_text(&self, file_name: &str) -> Result<(String, String), ReviewError> {
        read_text(&self.root, file_name)
    }

    /// 대기 중인 변경을 모두 적용한 파일 내용을 만든다.
    fn plan(&self, proposal: &Proposal) -> Result<PendingWrite, ReviewError> {
        let dir = self.dir();
        let config = self.config()?;
        match &proposal.change {
            Change::Create {
                file_name,
                content,
                location,
                ..
            } => plan_create(&self.root, &dir, &config, location, file_name, content),
            Change::Edit { file_name, regions } => {
                let pending: Vec<(usize, &EditRegion)> = regions
                    .iter()
                    .enumerate()
                    .filter(|(_, region)| region.review == Review::Pending)
                    .collect();
                plan_edit(&self.root, &dir, &config, file_name, &pending)
            }
        }
    }

    /// 제안서를 거부 상태로 기록한다. 편집안은 아직 대기 중인 영역을 모두 거부한다.
    /// 제안서 파일 외에는 아무것도 변경하지 않는다.
    pub fn reject(&self, id: &str, reason: &str) -> Result<Proposal, ReviewError> {
        let reason = valid_reason(reason)?;
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        let rejected = Review::Rejected {
            reason: reason.to_string(),
        };
        match &mut proposal.change {
            Change::Edit { regions, .. } if !regions.is_empty() => {
                for region in regions.iter_mut() {
                    if region.review == Review::Pending {
                        region.review = rejected.clone();
                    }
                }
                proposal.settle();
            }
            _ => proposal.review = rejected,
        }
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    /// 편집안의 영역 하나만 파일에 적용한다.
    pub fn approve_region(&self, id: &str, index: usize) -> Result<Proposal, ReviewError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        let config = self.config()?;
        let (file_name, region) = pending_region(&mut proposal, id, index)?;
        plan_edit(
            &self.root,
            &self.dir(),
            &config,
            &file_name,
            &[(index, &*region)],
        )?
        .commit()?;
        region.review = Review::Approved;
        proposal.settle();
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    /// 편집안의 영역 하나만 거부한다. 제안서 파일 외에는 아무것도 변경하지 않는다.
    pub fn reject_region(
        &self,
        id: &str,
        index: usize,
        reason: &str,
    ) -> Result<Proposal, ReviewError> {
        let reason = valid_reason(reason)?;
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        pending_region(&mut proposal, id, index)?.1.review = Review::Rejected {
            reason: reason.to_string(),
        };
        proposal.settle();
        self.save(id, &proposal)?;
        Ok(proposal)
    }

    /// 대기 중인 생성안의 내용을 사용자가 고친 값으로 바꾼다. 워크스페이스는 바꾸지 않는다.
    /// 저장한 뒤 승인했을 때 적용될 수 있는지 확인하고, 안 되면 그 이유를 함께 돌려준다.
    pub fn edit_content(&self, id: &str, content: &str) -> Result<Edited, ReviewError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        let Change::Create {
            content: current, ..
        } = &mut proposal.change
        else {
            return Err(ReviewError::NotFound(format!("{id} 생성안")));
        };
        let content = same_line_endings(content, current);
        if content != *current {
            *current = content;
            proposal.edited = true;
            self.save(id, &proposal)?;
        }
        Ok(self.edited(proposal))
    }

    /// 대기 중인 편집 영역의 편집 후 코드를 사용자가 고친 값으로 바꾼다. 워크스페이스는 바꾸지 않는다.
    /// 편집 전 코드는 파일과 맞춰 보는 기준이므로 고치지 않는다.
    pub fn edit_region(&self, id: &str, index: usize, after: &str) -> Result<Edited, ReviewError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut proposal = self.load_pending(id)?;
        let (_, region) = pending_region(&mut proposal, id, index)?;
        // 브라우저 입력란은 줄바꿈을 \n 으로 바꾸므로 원래 영역의 줄바꿈으로 되돌린다.
        let after = same_line_endings(after, &format!("{}{}", region.before, region.after));
        if after != region.after {
            region.after = after;
            region.edited = true;
            self.save(id, &proposal)?;
        }
        Ok(self.edited(proposal))
    }

    fn edited(&self, proposal: Proposal) -> Edited {
        let problem = self.plan(&proposal).err().map(|e| e.to_string());
        Edited { proposal, problem }
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

fn valid_reason(reason: &str) -> Result<&str, ReviewError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ReviewError::BlankReason);
    }
    Ok(reason)
}

/// 대기 중인 편집 영역과 그 파일명을 찾는다.
fn pending_region<'a>(
    proposal: &'a mut Proposal,
    id: &str,
    index: usize,
) -> Result<(String, &'a mut EditRegion), ReviewError> {
    let not_found = || ReviewError::NotFound(format!("{id} 영역 {}", index + 1));
    let Change::Edit { file_name, regions } = &mut proposal.change else {
        return Err(not_found());
    };
    let region = regions.get_mut(index).ok_or_else(not_found)?;
    if region.review != Review::Pending {
        return Err(ReviewError::AlreadyReviewed(format!(
            "{id} 영역 {}",
            index + 1
        )));
    }
    Ok((file_name.clone(), region))
}

/// 원래 텍스트가 CRLF를 쓰면 새 텍스트의 줄바꿈도 CRLF로, 아니면 LF로 맞춘다.
fn same_line_endings(text: &str, like: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if like.contains("\r\n") {
        text.replace('\n', "\r\n")
    } else {
        text
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
    let proposal: Proposal = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let task = proposal.task_of(&id);
    Ok(Entry { id, task, proposal })
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

    const TWO_REGIONS: &str = r#"{"kind":"edit","file_name":"src/main.rs","regions":[
        {"before":"one\n","after":"ONE\n","purpose":"a"},
        {"before":"three\n","after":"THREE\n","purpose":"b"}]}"#;

    fn two_regions() -> (tempfile::TempDir, Store, PathBuf) {
        let (dir, store) = repo();
        let file = dir.path().join("src/main.rs");
        fs::write(&file, "one\nthree\n").unwrap();
        write_proposal(dir.path(), "e", TWO_REGIONS);
        (dir, store, file)
    }

    fn region_reviews(proposal: &Proposal) -> Vec<Review> {
        match &proposal.change {
            Change::Edit { regions, .. } => regions.iter().map(|r| r.review.clone()).collect(),
            Change::Create { .. } => panic!("편집안이 아닙니다"),
        }
    }

    fn rejected(reason: &str) -> Review {
        Review::Rejected {
            reason: reason.to_string(),
        }
    }

    #[test]
    fn approving_one_region_applies_only_that_region() {
        let (_dir, store, file) = two_regions();
        let proposal = store.approve_region("e", 1).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\nTHREE\n");
        assert_eq!(proposal.review, Review::Pending);
        assert_eq!(
            region_reviews(&proposal),
            [Review::Pending, Review::Approved]
        );
        assert_eq!(store.list().unwrap().proposals[0].proposal, proposal);
    }

    #[test]
    fn rejecting_one_region_changes_no_code() {
        let (_dir, store, file) = two_regions();
        let proposal = store.reject_region("e", 0, " 불필요 ").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\nthree\n");
        assert_eq!(proposal.review, Review::Pending);
        assert_eq!(region_reviews(&proposal)[0], rejected("불필요"));
    }

    #[test]
    fn region_rejection_requires_a_reason() {
        let (_dir, store, _file) = two_regions();
        assert!(matches!(
            store.reject_region("e", 0, "  "),
            Err(ReviewError::BlankReason)
        ));
        assert_eq!(
            region_reviews(&store.list().unwrap().proposals[0].proposal)[0],
            Review::Pending
        );
    }

    #[test]
    fn proposal_result_follows_its_regions() {
        let (_dir, store, _file) = two_regions();
        store.approve_region("e", 0).unwrap();
        let proposal = store.approve_region("e", 1).unwrap();
        assert_eq!(proposal.review, Review::Approved);

        let (_dir, store, file) = two_regions();
        store.approve_region("e", 0).unwrap();
        let proposal = store.reject_region("e", 1, "사유").unwrap();
        assert_eq!(proposal.review, Review::PartiallyApproved);
        assert_eq!(fs::read_to_string(&file).unwrap(), "ONE\nthree\n");

        let (_dir, store, _file) = two_regions();
        store.reject_region("e", 0, "첫째").unwrap();
        let proposal = store.reject_region("e", 1, "둘째").unwrap();
        assert_eq!(proposal.review, rejected("영역 1: 첫째\n영역 2: 둘째"));
    }

    #[test]
    fn region_cannot_be_reviewed_twice() {
        let (_dir, store, file) = two_regions();
        store.approve_region("e", 0).unwrap();
        assert!(matches!(
            store.approve_region("e", 0),
            Err(ReviewError::AlreadyReviewed(_))
        ));
        assert!(matches!(
            store.reject_region("e", 0, "사유"),
            Err(ReviewError::AlreadyReviewed(_))
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "ONE\nthree\n");
    }

    #[test]
    fn unknown_region_is_not_found() {
        let (dir, store, _file) = two_regions();
        assert!(matches!(
            store.approve_region("e", 2),
            Err(ReviewError::NotFound(_))
        ));
        write_proposal(dir.path(), "c", CREATE);
        assert!(matches!(
            store.approve_region("c", 0),
            Err(ReviewError::NotFound(_))
        ));
        assert!(matches!(
            store.reject_region("c", 0, "사유"),
            Err(ReviewError::NotFound(_))
        ));
    }

    #[test]
    fn failed_region_stays_pending() {
        let (_dir, store, file) = two_regions();
        fs::write(&file, "changed\nthree\n").unwrap();
        assert!(matches!(
            store.approve_region("e", 0),
            Err(ReviewError::Apply(_))
        ));
        assert_eq!(
            region_reviews(&store.list().unwrap().proposals[0].proposal)[0],
            Review::Pending
        );
    }

    #[test]
    fn approving_whole_proposal_skips_reviewed_regions() {
        let (_dir, store, file) = two_regions();
        store.reject_region("e", 0, "사유").unwrap();
        let proposal = store.approve("e").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\nTHREE\n");
        assert_eq!(proposal.review, Review::PartiallyApproved);
    }

    #[test]
    fn rejecting_whole_proposal_keeps_approved_regions() {
        let (_dir, store, file) = two_regions();
        store.approve_region("e", 0).unwrap();
        let proposal = store.reject("e", "사유").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "ONE\nthree\n");
        assert_eq!(proposal.review, Review::PartiallyApproved);
        assert_eq!(region_reviews(&proposal)[1], rejected("사유"));
    }

    #[test]
    fn whole_approval_is_all_or_nothing() {
        let (_dir, store, file) = two_regions();
        fs::write(&file, "one\n").unwrap();
        assert!(matches!(store.approve("e"), Err(ReviewError::Apply(_))));
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\n");
        assert_eq!(
            region_reviews(&store.list().unwrap().proposals[0].proposal),
            [Review::Pending, Review::Pending]
        );
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
    fn task_comes_from_field_or_id() {
        let (dir, store) = store_with("20261102_1110_Siege_01", CREATE);
        write_proposal(dir.path(), "20261102_1110_Siege_02", CREATE);
        write_proposal(dir.path(), "solo", CREATE);
        write_proposal(dir.path(), "a_b_x1", CREATE);
        write_proposal(
            dir.path(),
            "named_03",
            &CREATE.replace(r#""kind""#, r#""task":" 버그 수정 ","kind""#),
        );
        let tasks: Vec<(String, String)> = store
            .list()
            .unwrap()
            .proposals
            .into_iter()
            .map(|entry| (entry.id, entry.task))
            .collect();
        let expected = [
            ("20261102_1110_Siege_01", "20261102_1110_Siege"),
            ("20261102_1110_Siege_02", "20261102_1110_Siege"),
            ("a_b_x1", "a_b_x1"),
            ("named_03", "버그 수정"),
            ("solo", "solo"),
        ];
        assert_eq!(
            tasks,
            expected.map(|(id, task)| (id.to_string(), task.to_string()))
        );
    }

    #[test]
    fn edited_region_is_saved_with_file_line_endings() {
        let (_dir, store, file) = two_regions();
        let edited = store.edit_region("e", 1, "THREE!\n").unwrap();
        assert_eq!(edited.problem, None);
        let Change::Edit { regions, .. } = &edited.proposal.change else {
            panic!("편집안이 아닙니다");
        };
        assert_eq!(regions[1].after, "THREE!\n");
        assert!(regions[1].edited && !regions[0].edited);
        assert_eq!(fs::read_to_string(&file).unwrap(), "one\nthree\n");

        store.approve("e").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "ONE\nTHREE!\n");

        let (dir, store, _file) = cp949_repo(None);
        write_proposal(dir.path(), "e", &cpp_edit_json("int a;\r\n", "int b;\r\n"));
        let edited = store.edit_region("e", 0, "int c;\nint d;\n").unwrap();
        let Change::Edit { regions, .. } = &edited.proposal.change else {
            panic!("편집안이 아닙니다");
        };
        assert_eq!(regions[0].after, "int c;\r\nint d;\r\n");
    }

    #[test]
    fn edit_reports_problem_but_keeps_text() {
        let (dir, store, _file) = cp949_repo(None);
        write_proposal(dir.path(), "e", &cpp_edit_json("int a;", "int b;"));
        let edited = store.edit_region("e", 0, "int b; // 😀").unwrap();
        assert!(edited.problem.is_some());
        let saved = &store.list().unwrap().proposals[0].proposal;
        let Change::Edit { regions, .. } = &saved.change else {
            panic!("편집안이 아닙니다");
        };
        assert_eq!(regions[0].after, "int b; // 😀");
        assert_eq!(saved.review, Review::Pending);
    }

    #[test]
    fn edited_content_is_saved_and_unchanged_text_is_not_marked() {
        let (dir, store) = repo();
        write_proposal(dir.path(), "c", &create_json("src/", "n.rs"));
        let original = fs::read_to_string(proposal_path(dir.path(), "c")).unwrap();
        assert!(!store.edit_content("c", "new\n").unwrap().proposal.edited);
        assert_eq!(
            fs::read_to_string(proposal_path(dir.path(), "c")).unwrap(),
            original
        );

        let edited = store.edit_content("c", "changed\n").unwrap();
        assert!(edited.proposal.edited);
        store.approve("c").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("src/n.rs")).unwrap(),
            "changed\n"
        );
    }

    #[test]
    fn only_pending_items_can_be_edited() {
        let (_dir, store, _file) = two_regions();
        store.approve_region("e", 0).unwrap();
        assert!(matches!(
            store.edit_region("e", 0, "x"),
            Err(ReviewError::AlreadyReviewed(_))
        ));
        assert!(matches!(
            store.edit_region("e", 5, "x"),
            Err(ReviewError::NotFound(_))
        ));
        assert!(matches!(
            store.edit_content("e", "x"),
            Err(ReviewError::NotFound(_))
        ));
        store.reject("e", "사유").unwrap();
        assert!(matches!(
            store.edit_region("e", 1, "x"),
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

    /// CP949 + CRLF로 된 `src/main.cpp`가 있는 워크스페이스를 만든다.
    fn cp949_repo(config: Option<&str>) -> (tempfile::TempDir, Store, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        let file = root.join("src/main.cpp");
        // "// 한글\r\nint a;\r\n"
        fs::write(&file, b"// \xC7\xD1\xB1\xDB\r\nint a;\r\n").unwrap();
        if let Some(config) = config {
            fs::create_dir_all(root.join(".syntra")).unwrap();
            fs::write(root.join(CONFIG_FILE), config).unwrap();
        }
        let store = Store::new(root);
        (dir, store, file)
    }

    fn cpp_edit_json(before: &str, after: &str) -> String {
        edit_json(before, after).replace("src/main.rs", "src/main.cpp")
    }

    #[test]
    fn cp949_crlf_file_keeps_its_format() {
        let (dir, store, file) = cp949_repo(None);
        write_proposal(
            dir.path(),
            "e",
            &cpp_edit_json("// 한글\nint a;\n", "// 한글 수정\nint a = 0;\n"),
        );
        assert_eq!(store.approve("e").unwrap().review, Review::Approved);
        assert_eq!(
            fs::read(&file).unwrap(),
            b"// \xC7\xD1\xB1\xDB \xBC\xF6\xC1\xA4\r\nint a = 0;\r\n"
        );
    }

    #[test]
    fn unmappable_character_leaves_cp949_file_untouched() {
        let (dir, store, file) = cp949_repo(None);
        write_proposal(dir.path(), "e", &cpp_edit_json("int a;", "int a; // 😀"));
        assert!(matches!(store.approve("e"), Err(ReviewError::Apply(_))));
        assert_eq!(
            fs::read(&file).unwrap(),
            b"// \xC7\xD1\xB1\xDB\r\nint a;\r\n"
        );
    }

    #[test]
    fn mixed_line_endings_need_an_exact_match() {
        let (dir, store, file) = cp949_repo(None);
        fs::write(&file, "a\r\nb\nc\r\n").unwrap();
        write_proposal(dir.path(), "e", &cpp_edit_json("a\nb\n", "x\n"));
        let Err(ReviewError::Apply(message)) = store.approve("e") else {
            panic!("줄바꿈이 다른데 적용되었습니다");
        };
        assert!(message.contains("줄바꿈"), "{message}");

        write_proposal(dir.path(), "e", &cpp_edit_json("a\r\nb\n", "x\r\n"));
        store.approve("e").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "x\r\nc\r\n");
    }

    #[test]
    fn create_follows_the_config() {
        let (dir, store, _file) = cp949_repo(Some(r#"{"encoding":"cp949","line_ending":"crlf"}"#));
        write_proposal(
            dir.path(),
            "c",
            &serde_json::json!({
                "kind": "create",
                "file_name": "new.h",
                "content": "// 한글\n#pragma once\n",
                "purpose": "p",
                "location": "src/",
            })
            .to_string(),
        );
        store.approve("c").unwrap();
        assert_eq!(
            fs::read(dir.path().join("src/new.h")).unwrap(),
            b"// \xC7\xD1\xB1\xDB\r\n#pragma once\r\n"
        );
    }

    #[test]
    fn ascii_file_gets_the_configured_encoding() {
        let (dir, store, file) = cp949_repo(Some(r#"{"encoding":"cp949"}"#));
        fs::write(&file, "int a;\r\n").unwrap();
        write_proposal(
            dir.path(),
            "e",
            &cpp_edit_json("int a;\n", "int a; // 한글\n"),
        );
        store.approve("e").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"int a; // \xC7\xD1\xB1\xDB\r\n");
    }

    #[test]
    fn broken_config_refuses_approval() {
        let (dir, store, file) = cp949_repo(Some(r#"{"encoding":"euc-kr"}"#));
        write_proposal(dir.path(), "e", &cpp_edit_json("int a;", "int b;"));
        assert!(matches!(store.approve("e"), Err(ReviewError::Config(_))));
        assert!(matches!(store.check("e"), Err(ReviewError::Config(_))));
        assert_eq!(
            fs::read(&file).unwrap(),
            b"// \xC7\xD1\xB1\xDB\r\nint a;\r\n"
        );
    }

    #[test]
    fn check_reports_without_changing_anything() {
        let (dir, store, file) = cp949_repo(Some(r#"{"encoding":"cp949"}"#));
        let original = fs::read(&file).unwrap();
        write_proposal(
            dir.path(),
            "ok",
            &cpp_edit_json("int a;\n", "int b; // 한글\n"),
        );
        write_proposal(dir.path(), "missing", &cpp_edit_json("int z;", "int b;"));
        write_proposal(
            dir.path(),
            "emoji",
            &cpp_edit_json("int a;", "int a; // 😀"),
        );
        write_proposal(dir.path(), "exists", &create_json("src/", "main.cpp"));
        let before = fs::read_to_string(proposal_path(dir.path(), "ok")).unwrap();

        store.check("ok").unwrap();
        for id in ["missing", "emoji", "exists"] {
            assert!(
                matches!(store.check(id), Err(ReviewError::Apply(_))),
                "{id}"
            );
        }

        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(
            fs::read_to_string(proposal_path(dir.path(), "ok")).unwrap(),
            before
        );
    }

    #[test]
    fn read_text_matches_what_approval_compares() {
        let (dir, store, _file) = cp949_repo(None);
        fs::write(dir.path().join("src/gbk.h"), b"// \xC9\xCF\r\nint a;\r\n").unwrap();
        let (format, text) = store.read_text("src/main.cpp").unwrap();
        assert_eq!(format, "CP949, CRLF");
        assert_eq!(text, "// 한글\r\nint a;\r\n");
        let (format, text) = store.read_text("src/gbk.h").unwrap();
        assert!(format.contains("해석되지 않는"), "{format}");
        assert_eq!(text, "// \u{FFFD}\u{FFFD}\r\nint a;\r\n");
        assert!(matches!(
            store.read_text("../outside.txt"),
            Err(ReviewError::UnsafePath(_))
        ));
    }
}
