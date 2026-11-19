/// Settings read once when the engine starts.
pub struct EngineConfig {
    pub queue_depth_limit: usize,
    pub worker_count: u32,
    pub lease_seconds: u64,
    pub listen_address: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            queue_depth_limit: 10_000,
            worker_count: 4,
            lease_seconds: 30,
            listen_address: "127.0.0.1:7400".to_owned(),
        }
    }
}

/// Parses `key = value` lines; unknown keys are ignored.
pub fn load_config(source: &str) -> EngineConfig {
    let mut config = EngineConfig::default();
    for line in source.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "queue_depth_limit" => config.queue_depth_limit = value.trim().parse().unwrap_or(10_000),
            "worker_count" => config.worker_count = value.trim().parse().unwrap_or(4),
            "lease_seconds" => config.lease_seconds = value.trim().parse().unwrap_or(30),
            "listen_address" => config.listen_address = value.trim().to_owned(),
            _ => {}
        }
    }
    config
}
