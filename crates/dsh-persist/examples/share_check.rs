fn main() {
    let root = dirs_home().join("sessions");
    let recorder = dsh_persist::SessionRecorder::new(root.clone());
    println!("root = {}", root.display());
    match recorder.list() {
        Ok(list) => {
            for s in &list {
                let cwd = s.cwd.clone().unwrap_or_default();
                let title = recorder.title_of(&s.id, cwd.as_str()).unwrap_or_default();
                println!("{} | cwd={} | modified={:?} | title={:?}", s.id.as_str(), cwd, s.modified, title);
            }
            println!("total = {}", list.len());
        }
        Err(e) => println!("list failed: {e}"),
    }
}
fn dirs_home() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("DSH_HOME") { if !p.trim().is_empty() { return p.into(); } }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".dsh")
}
