use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

const MAX_RECENT: usize = 8;

/// 폴더 선택 화면에 보여줄 디렉터리 정보.
#[derive(Debug, Serialize)]
pub struct Folder {
    pub path: String,
    pub parent: Option<String>,
    pub dirs: Vec<String>,
    pub drives: Vec<String>,
    pub is_git: bool,
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

pub fn browse(path: &Path) -> io::Result<Folder> {
    let path = std::path::absolute(path)?;
    let mut dirs: Vec<String> = fs::read_dir(&path)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    dirs.sort_by_key(|name| name.to_lowercase());
    Ok(Folder {
        parent: path.parent().map(|p| p.display().to_string()),
        drives: drives(),
        is_git: path.join(".git").exists(),
        path: path.display().to_string(),
        dirs,
    })
}

fn drives() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", letter as char))
        .filter(|drive| Path::new(drive).exists())
        .collect()
}

/// 최근 연 워크스페이스 목록. 편의 기능이므로 저장에 실패해도 무시한다.
pub struct Recent {
    file: Option<PathBuf>,
}

impl Recent {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self { file }
    }

    pub fn list(&self) -> Vec<String> {
        let saved: Vec<String> = self
            .file
            .as_ref()
            .and_then(|file| fs::read_to_string(file).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        saved
            .into_iter()
            .filter(|path| Path::new(path).is_dir())
            .collect()
    }

    pub fn add(&self, path: &str) {
        let Some(file) = &self.file else { return };
        let mut paths = self.list();
        paths.retain(|existing| existing != path);
        paths.insert(0, path.to_string());
        paths.truncate(MAX_RECENT);
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&paths) {
            let _ = fs::write(file, json);
        }
    }
}
