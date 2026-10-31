//! 예제 제안서가 들어 있는 체험용 워크스페이스를 `target/demo-workspace`에 만든다.

use std::fs;
use std::path::Path;
use std::process::Command;

const MAIN_RS: &str = "fn main() {\n    println!(\"안녕하세요, 철수님!\");\n}\n";
const PROPOSALS: [(&str, &str); 2] = [
    (
        "create-greeting.json",
        include_str!("../../../examples/proposals/create-greeting.json"),
    ),
    (
        "edit-main.json",
        include_str!("../../../examples/proposals/edit-main.json"),
    ),
];

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("git을 실행할 수 없습니다");
    assert!(status.success(), "git {args:?} 실패");
}

fn main() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/demo-workspace");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), MAIN_RS)?;
    fs::write(root.join(".gitignore"), "/.syntra\n")?;

    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "SynTra Demo"]);
    git(&root, &["config", "user.email", "demo@example.com"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "데모 시작"]);

    let proposals = root.join(".syntra/proposals");
    fs::create_dir_all(&proposals)?;
    for (name, json) in PROPOSALS {
        fs::write(proposals.join(name), json)?;
    }

    println!("{}", root.canonicalize()?.display());
    Ok(())
}
