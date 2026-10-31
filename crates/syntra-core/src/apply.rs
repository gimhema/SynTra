use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::model::EditRegion;
use crate::store::ReviewError;

/// 승인된 생성안을 워크스페이스에 배치한다.
pub(crate) fn apply_create(
    root: &Path,
    proposals: &Path,
    location: &str,
    file_name: &str,
    content: &str,
) -> Result<(), ReviewError> {
    let relative = Path::new(location).join(file_name);
    create(&relative, &safe_path(root, proposals, &relative)?, content)
}

/// 승인된 편집 영역들을 파일에 적용한다. 하나라도 실패하면 파일을 바꾸지 않는다.
/// 영역은 (제안서 내 번호, 영역) 쌍으로 받아 오류에 원래 번호를 표시한다.
pub(crate) fn apply_edit(
    root: &Path,
    proposals: &Path,
    file_name: &str,
    regions: &[(usize, &EditRegion)],
) -> Result<(), ReviewError> {
    let relative = Path::new(file_name);
    edit(relative, &safe_path(root, proposals, relative)?, regions)
}

fn create(relative: &Path, target: &Path, content: &str) -> Result<(), ReviewError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => {
                ReviewError::Apply(format!("이미 존재하는 파일입니다: {}", relative.display()))
            }
            _ => e.into(),
        })?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn edit(
    relative: &Path,
    target: &Path,
    regions: &[(usize, &EditRegion)],
) -> Result<(), ReviewError> {
    let original = fs::read_to_string(target).map_err(|e| {
        ReviewError::Apply(format!(
            "파일을 읽을 수 없습니다 ({}): {e}",
            relative.display()
        ))
    })?;
    fs::write(target, replace_regions(&original, regions)?)?;
    Ok(())
}

/// 영역마다 편집 전 코드가 정확히 한 곳에서 발견될 때만 교체한다.
fn replace_regions(
    original: &str,
    regions: &[(usize, &EditRegion)],
) -> Result<String, ReviewError> {
    let mut text = original.to_string();
    for (index, region) in regions {
        let number = index + 1;
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

/// 제안서의 경로는 에이전트가 작성한 값이므로 워크스페이스 밖이나 관리 영역을 가리키지 못하게 막는다.
fn safe_path(root: &Path, proposals: &Path, relative: &Path) -> Result<PathBuf, ReviewError> {
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
    // 제안서가 다른 제안서나 자신의 검토 결과를 고치지 못하게 한다.
    if let Ok(canonical_proposals) = proposals.canonicalize()
        && canonical.starts_with(&canonical_proposals)
    {
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
