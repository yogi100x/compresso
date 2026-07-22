use compresso_core::operation_identity;

#[test]
fn test_public_operation_identity_is_ulid_compatible_and_unique() {
    let first = operation_identity();
    let second = operation_identity();

    assert_ne!(first.id, second.id);
    for id in [&first.id, &second.id] {
        assert_eq!(id.len(), 26);
        assert!(matches!(id.as_bytes()[0], b'0'..=b'7'));
        assert!(
            id.bytes()
                .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
        );
    }
    assert!(first.created_at > 0);
    assert!(second.created_at >= first.created_at);
}
