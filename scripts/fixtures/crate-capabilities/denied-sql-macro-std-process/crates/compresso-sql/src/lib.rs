macro_rules! p { ($a:ident) => { std::$a::Command::new("sh") } } pub fn f() { let _ = p!(process); }
