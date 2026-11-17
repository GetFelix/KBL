use kbl_core::content::Content;

use super::Texts;

fn text(n: usize) -> Content {
    Content::Text("x".repeat(n))
}

#[test]
fn the_least_recently_used_text_goes_first_and_the_size_stays_under_the_cap() {
    let mut t = Texts::with_cap(100);
    t.insert(1, text(40));
    t.insert(2, text(40));
    // Using 1 makes 2 the oldest.
    assert!(t.get(1).is_some());
    t.insert(3, text(40));
    assert!(t.peek(2).is_none());
    assert!(t.peek(1).is_some() && t.peek(3).is_some());
    assert!(t.bytes() <= 100);
}

#[test]
fn a_text_over_the_cap_alone_is_kept_and_a_replaced_one_is_not_counted_twice() {
    let mut t = Texts::with_cap(10);
    t.insert(1, text(50));
    assert!(t.peek(1).is_some());
    t.insert(1, text(5));
    assert_eq!(t.bytes(), 5);
}
