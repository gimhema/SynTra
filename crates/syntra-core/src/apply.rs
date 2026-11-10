use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::model::EditRegion;
use crate::store::ReviewError;
use crate::text::{self, Config, FileLineEnding, LineEnding, SourceText};

/// 디스크에 쓸 내용. 검사만 할 때는 `commit`하지 않고 버린다.
pub(crate) struct PendingWrite {
    relative: PathBuf,
    target: PathBuf,
    bytes: Vec<u8>,
    new_file: bool,
}

impl PendingWrite {
    pub fn commit(self) -> Result<(), ReviewError> {
        if self.new_file {
            create(&self.relative, &self.target, &self.bytes)
        } else {
            fs::write(&self.target, &self.bytes)?;
            Ok(())
        }
    }
}

/// 생성안을 워크스페이스에 배치할 내용을 만든다. 인코딩과 줄바꿈은 설정을 따른다.
pub(crate) fn plan_create(
    root: &Path,
    proposals: &Path,
    config: &Config,
    location: &str,
    file_name: &str,
    content: &str,
) -> Result<PendingWrite, ReviewError> {
    let relative = Path::new(location).join(file_name);
    let target = safe_path(root, proposals, &relative)?;
    if target.exists() {
        return Err(already_exists(&relative));
    }
    let content = match config.line_ending {
        Some(line_ending) => Cow::Owned(line_ending.apply(content)),
        None => Cow::Borrowed(content),
    };
    let bytes = text::encode(&content, config.encoding)
        .map_err(|e| ReviewError::Apply(format!("{}: {e}", relative.display())))?;
    Ok(PendingWrite {
        relative,
        target,
        bytes,
        new_file: true,
    })
}

/// 편집 영역들을 적용한 파일 내용을 만든다. 하나라도 실패하면 아무것도 돌려주지 않는다.
/// 영역은 (제안서 내 번호, 영역) 쌍으로 받아 오류에 원래 번호를 표시한다.
pub(crate) fn plan_edit(
    root: &Path,
    proposals: &Path,
    config: &Config,
    file_name: &str,
    regions: &[(usize, &EditRegion)],
) -> Result<PendingWrite, ReviewError> {
    let relative = Path::new(file_name);
    let target = safe_path(root, proposals, relative)?;
    let bytes = edit(relative, &target, config, regions)?;
    Ok(PendingWrite {
        relative: relative.to_path_buf(),
        target,
        bytes,
        new_file: false,
    })
}

fn already_exists(relative: &Path) -> ReviewError {
    ReviewError::Apply(format!("이미 존재하는 파일입니다: {}", relative.display()))
}

fn create(relative: &Path, target: &Path, content: &[u8]) -> Result<(), ReviewError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => already_exists(relative),
            _ => e.into(),
        })?;
    file.write_all(content)?;
    Ok(())
}

fn edit(
    relative: &Path,
    target: &Path,
    config: &Config,
    regions: &[(usize, &EditRegion)],
) -> Result<Vec<u8>, ReviewError> {
    let read_error = |e: String| {
        ReviewError::Apply(format!(
            "파일을 읽을 수 없습니다 ({}): {e}",
            relative.display()
        ))
    };
    let bytes = fs::read(target).map_err(|e| read_error(e.to_string()))?;
    let source = SourceText::decode(&bytes).map_err(read_error)?;
    let line_ending = FileLineEnding::detect(&source.text);
    let edited = replace_regions(&source.text, regions, line_ending, config)?;
    source
        .encode(&edited, config.encoding)
        .map_err(|e| ReviewError::Apply(format!("{}: {e}", relative.display())))
}

/// 파일을 SynTra가 비교에 쓰는 그대로 읽는다. 제안서의 `before`를 옮겨 적을 때 쓴다.
pub(crate) fn read_text(root: &Path, file_name: &str) -> Result<(String, String), ReviewError> {
    let relative = Path::new(file_name);
    let target = safe_path(root, Path::new(""), relative)?;
    let bytes = fs::read(&target)?;
    let source = SourceText::decode(&bytes)
        .map_err(|e| ReviewError::Apply(format!("{}: {e}", relative.display())))?;
    let line_ending = match FileLineEnding::detect(&source.text) {
        FileLineEnding::Uniform(LineEnding::Crlf) => "CRLF",
        FileLineEnding::Uniform(LineEnding::Lf) => "LF",
        FileLineEnding::Mixed => "혼합",
        FileLineEnding::Absent => "없음",
    };
    Ok((
        format!("{}, {line_ending}", source.encoding_name()),
        text::for_display(&source.text),
    ))
}

/// 영역마다 편집 전 코드가 정확히 한 곳에서 발견될 때만 교체한다.
/// 파일이 한 가지 줄바꿈만 쓰면 제안서의 줄바꿈을 파일에 맞춘 뒤 비교한다.
fn replace_regions(
    original: &str,
    regions: &[(usize, &EditRegion)],
    line_ending: FileLineEnding,
    config: &Config,
) -> Result<String, ReviewError> {
    let target = line_ending.target(config);
    let convert = |text: &'_ str| -> String {
        target.map_or_else(|| text.to_string(), |target: LineEnding| target.apply(text))
    };
    let mut text = original.to_string();
    for (index, region) in regions {
        let number = index + 1;
        if region.before.is_empty() {
            return Err(ReviewError::Apply(format!(
                "영역 {number}: 편집 전 코드가 비어 있습니다"
            )));
        }
        let before = convert(&region.before);
        match text.matches(&before).count() {
            1 => text = text.replacen(&before, &convert(&region.after), 1),
            0 => {
                let hint = if line_ending == FileLineEnding::Mixed && before.contains('\n') {
                    " (파일의 줄바꿈이 줄마다 달라 편집 전 코드의 줄바꿈까지 파일과 같아야 합니다)"
                } else {
                    ""
                };
                return Err(ReviewError::Apply(format!(
                    "영역 {number}: 편집 전 코드를 파일에서 찾을 수 없습니다{hint}"
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
