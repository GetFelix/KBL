use super::*;

#[test]
fn plain_http_listens_only_on_loopback_without_a_tls_proxy() {
    let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
    for ok in [
        "127.0.0.1:8790",
        "127.0.0.2:8790",
        "[::1]:8790",
        "[::ffff:127.0.0.1]:8790",
    ] {
        check_listen(addr(ok), false).unwrap();
    }
    for open in [
        "0.0.0.0:8790",
        "10.0.0.5:8790",
        "169.254.1.1:8790",
        "[::]:8790",
        "[fe80::1]:8790",
    ] {
        assert!(check_listen(addr(open), false).is_err(), "{open}");
        check_listen(addr(open), true).unwrap();
    }
}
