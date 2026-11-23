macro_rules! mk04 { ($a:ident, $b:ident) => { $a::$b::Command::new("sh") } } pub fn e04() { let _ = mk04!(tokio, process); }
