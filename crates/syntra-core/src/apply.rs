use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::model::{Change, EditRegion};
use crate::store::ReviewError;

/// 승인된 변경을 워크스페이스에 적용하고, 커밋이 생겼다면 그 해시를 돌려준다.
pub(crate) fn apply(root: &Path, change: &Change) -> Result<Option<String>, ReviewError> {
    match change {
        Change::Create {
            file_name,
            content,
            location,
            ..
        } => {
            create(root, &Path::new(location).join(file_name), content)?;
            Ok(None)
        }
        Change::Edit { file_name, regions } => edit(root, Path::new(file_name), regions).map(Some),
    }
}

fn create(root: &Path, relative: &Path, content: &str) -> Result<(), ReviewError> {
    let target = safe_path(root, relative)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => {
                ReviewError::Apply(format!("이미 존재하는 파일입니다: {}", relative.display()))
            }
            _ => e.into(),
        })?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn edit(root: &Path, relative: &Path, regions: &[EditRegion]) -> Result<String, ReviewError> {
    let target = safe_path(root, relative)?;
    let original = fs::read_to_string(&target).map_err(|e| {
        ReviewError::Apply(format!(
            "파일을 읽을 수 없습니다 ({}): {e}",
            relative.display()
        ))
    })?;
    let edited = replace_regions(&original, regions)?;

    // 사용자가 작업 중인 변경이 커밋에 섞이지 않도록 깨끗한 파일만 편집한다.
    let status = git(
        root,
        &[
            "status".as_ref(),
            "--porcelain".as_ref(),
            "--".as_ref(),
            relative.as_os_str(),
        ],
    )?;
    if !status.trim().is_empty() {
        return Err(ReviewError::Apply(format!(
            "커밋되지 않은 변경이 있어 적용할 수 없습니다: {}",
            relative.display()
        )));
    }

    fs::write(&target, edited)?;
    let message = commit_message(relative, regions);
    let committed = git(
        root,
        &[
            "commit".as_ref(),
            "--only".as_ref(),
            "-m".as_ref(),
            message.as_ref(),
            "--".as_ref(),
            relative.as_os_str(),
        ],
    )
    .and_then(|_| git(root, &["rev-parse".as_ref(), "HEAD".as_ref()]));
    match committed {
        Ok(hash) => Ok(hash.trim().to_string()),
        Err(error) => {
            fs::write(&target, original)?;
            Err(error)
        }
    }
}

/// 영역마다 편집 전 코드가 정확히 한 곳에서 발견될 때만 교체한다.
fn replace_regions(original: &str, regions: &[EditRegion]) -> Result<String, ReviewError> {
    let mut text = original.to_string();
    for (i, region) in regions.iter().enumerate() {
        let number = i + 1;
        if region.before.is_empty() {
            return Err(ReviewError::Apply(format!(
                "영역 {number}: 편집 전 코드가 비어 있습니다"
            )));
        }
        match text.matches(&region.before).count() {
            1 => text = text.replacen(&region.before, &region.after, 1),
            0 => {
                return Err(ReviewError::Apply(format!(
                    "영역 {number}: 편집 전 코드를 파일에서 찾을 수 없습니다"
                )));
            }
            count => {
                return Err(ReviewError::Apply(format!(
                    "영역 {number}: 편집 전 코드가 {count}곳에서 발견되어 위치를 특정할 수 없습니다"
                )));
            }
        }
    }
    Ok(text)
}

fn commit_message(relative: &Path, regions: &[EditRegion]) -> String {
    let mut message = format!("SynTra: {} 편집\n\n", relative.display());
    for region in regions {
        message.push_str(&format!("- {}\n", region.purpose));
    }
    message
}

fn git(root: &Path, args: &[&OsStr]) -> Result<String, ReviewError> {
    let output = Command::new("git")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| ReviewError::Apply(format!("git을 실행할 수 없습니다: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        Err(ReviewError::Apply(format!("git 오류: {}", detail.trim())))
    }
}

/// 제안서의 경로는 에이전트가 작성한 값이므로 워크스페이스 밖이나 관리 영역을 가리키지 못하게 막는다.
fn safe_path(root: &Path, relative: &Path) -> Result<PathBuf, ReviewError> {
    let unsafe_path = || ReviewError::UnsafePath(relative.display().to_string());

    let mut has_name = false;
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) if is_allowed_name(name) => has_name = true,
            _ => return Err(unsafe_path()),
        }
    }
    if !has_name {
        return Err(unsafe_path());
    }

    // 심볼릭 링크로 밖을 가리키는 경우까지 막기 위해 이미 존재하는 가장 깊은 경로를 확인한다.
    let target = root.join(relative);
    let canonical_root = root.canonicalize()?;
    let mut existing = target.as_path();
    while existing.symlink_metadata().is_err() {
        existing = existing.parent().ok_or_else(unsafe_path)?;
    }
    let canonical = existing.canonicalize().map_err(|_| unsafe_path())?;
    if !canonical.starts_with(&canonical_root) {
        return Err(unsafe_path());
    }
    Ok(target)
}

fn is_allowed_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    // Windows는 이름 끝의 점과 공백을 무시하고, ':'는 대체 데이터 스트림을 가리킨다.
    let trimmed = name.trim_end_matches(['.', ' ']);
    !name.contains(':')
        && !trimmed.eq_ignore_ascii_case(".git")
        && !trimmed.eq_ignore_ascii_case(".syntra")
}
