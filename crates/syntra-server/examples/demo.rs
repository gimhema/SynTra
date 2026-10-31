//! 예제 제안서가 들어 있는 체험용 워크스페이스를 `target/demo-workspace`에 만든다.

use std::fs;
use std::path::Path;

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

fn main() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/demo-workspace");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), MAIN_RS)?;

    let proposals = root.join(".syntra/proposals");
    fs::create_dir_all(&proposals)?;
    for (name, json) in PROPOSALS {
        fs::write(proposals.join(name), json)?;
    }

    println!("{}", root.canonicalize()?.display());
    Ok(())
}
