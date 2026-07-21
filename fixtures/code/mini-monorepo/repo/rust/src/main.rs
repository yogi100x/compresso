mod session;

use session::SessionManager;

fn main() {
    let mut manager = SessionManager::new(8);
    let id = manager.start("indexing");
    println!("started session {id}");
    manager.end(id);
    println!("active sessions: {}", manager.active_count());
}
