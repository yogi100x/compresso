use crate::lib::Library;

fn main() {
    let library = Library::new("fixture".to_owned());
    println!("{}", library.name);
}
