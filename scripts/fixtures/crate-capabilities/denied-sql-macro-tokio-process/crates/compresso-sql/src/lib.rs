macro_rules! p { ($a:ident) => { tokio::$a::Command::new("sh") } } pub fn f() { let _ = p!(process); }
