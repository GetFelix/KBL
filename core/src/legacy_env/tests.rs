use super::*;

fn var(k: &str, v: &str) -> (String, OsString) {
    (k.to_owned(), v.into())
}

#[test]
fn old_names_are_copied_and_new_names_win() {
    let vars = [
        var("AGENTS_RUN", "old"),
        var("AGENTS_TENANT", "old"),
        var("KBL_TENANT", "new"),
        var("PATH", "x"),
    ];
    assert_eq!(renamed(&vars), vec![var("KBL_RUN", "old")]);
}
