//! 에이전트가 제안서를 넘기기 전에 쓰는 명령. 워크스페이스는 바꾸지 않는다.

use std::path::Path;
use std::process::ExitCode;

use syntra_core::{Review, Store};

pub const USAGE: &str = "\
사용법:
  syntra [--no-open]                          검토 화면 서버 실행
  syntra check <워크스페이스> [제안서 id...]  대기 중인 제안서가 승인 시 적용되는지 검사
  syntra cat <워크스페이스> <파일> [시작줄] [끝줄]
                                              파일을 승인 시 비교하는 그대로 출력";

/// 대기 중인 제안서를 모두(또는 지정한 것만) 검사한다. 하나라도 실패하면 종료 코드 1.
pub fn check(args: &[String]) -> ExitCode {
    let Some((workspace, ids)) = args.split_first() else {
        return usage();
    };
    let store = Store::new(Store::workspace_root(Path::new(workspace)));
    let listing = match store.list() {
        Ok(listing) => listing,
        Err(e) => return fail(&e.to_string()),
    };
    let mut failed = 0;
    let mut checked = 0;
    for invalid in &listing.invalid {
        if ids.is_empty() || ids.iter().any(|id| invalid.file == format!("{id}.json")) {
            println!("실패 {}: {}", invalid.file, invalid.error);
            failed += 1;
            checked += 1;
        }
    }
    let targets: Vec<&str> = if ids.is_empty() {
        listing
            .proposals
            .iter()
            .filter(|entry| entry.proposal.review == Review::Pending)
            .map(|entry| entry.id.as_str())
            .collect()
    } else {
        ids.iter().map(String::as_str).collect()
    };
    for id in &targets {
        checked += 1;
        match store.check(id) {
            Ok(()) => println!("통과 {id}"),
            Err(e) => {
                println!("실패 {id}: {e}");
                failed += 1;
            }
        }
    }
    println!("{}: {checked}건 검사, {failed}건 실패", listing.source);
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// 파일의 형식을 표준 오류로, 내용을 표준 출력(UTF-8)으로 내보낸다.
pub fn cat(args: &[String]) -> ExitCode {
    let [workspace, file, range @ ..] = args else {
        return usage();
    };
    let line_number = |index: usize| -> Option<usize> {
        range.get(index).map(|value| value.parse().unwrap_or(0))
    };
    let (start, end) = (line_number(0).unwrap_or(1), line_number(1));
    if start == 0 || end == Some(0) || range.len() > 2 {
        return usage();
    }
    let store = Store::new(Store::workspace_root(Path::new(workspace)));
    let (format, text) = match store.read_text(file) {
        Ok(result) => result,
        Err(e) => return fail(&e.to_string()),
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let end = end.unwrap_or(lines.len()).min(lines.len());
    eprintln!("{file}: {format}, {}줄 중 {start}-{end}줄", lines.len());
    if start <= end {
        print!("{}", lines[start - 1..end].concat());
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}
