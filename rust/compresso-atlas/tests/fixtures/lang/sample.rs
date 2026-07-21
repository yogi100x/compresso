use crate::graph::ContextGraph;
use serde::{Serialize, Deserialize};
pub struct SessionManager {
    session_id: String,
    active: bool,
    retries: usize,
}

impl SessionManager {
    pub fn start(&self) -> Result<(), Error> {
        let _ = &self.session_id;
        let _ = self.retries;
        Ok(())
    }
}

pub fn helper() {
    println!("helper");
}
