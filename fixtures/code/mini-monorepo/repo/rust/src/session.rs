use std::collections::HashMap;

pub struct SessionManager {
    max_sessions: usize,
    active: HashMap<u64, String>,
    next_id: u64,
}

impl SessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions,
            active: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn start(&mut self, label: &str) -> u64 {
        assert!(
            self.active.len() < self.max_sessions,
            "session limit reached"
        );
        let id = self.next_id;
        self.next_id += 1;
        self.active.insert(id, label.to_string());
        id
    }

    pub fn end(&mut self, id: u64) -> bool {
        self.active.remove(&id).is_some()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}
