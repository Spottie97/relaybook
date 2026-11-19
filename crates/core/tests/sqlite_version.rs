#[test]
fn bundled_sqlite_is_3_54_1() {
    assert_eq!(rusqlite::version(), "3.54.1");
}
