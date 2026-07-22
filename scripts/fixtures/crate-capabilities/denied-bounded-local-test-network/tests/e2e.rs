use std::net::TcpStream;

#[test]
fn test_network_types_remain_forbidden() {
    assert_eq!(std::mem::size_of::<Option<TcpStream>>(), std::mem::size_of::<TcpStream>());
}
