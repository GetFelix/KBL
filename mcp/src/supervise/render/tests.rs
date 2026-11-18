use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::supervise::config::COMMAND_VARS;

fn vars<'a>(model: Option<&'a str>) -> Vars<'a> {
    Vars {
        model,
        worker: "sup-w1",
        mcp_config_path: Some("/s/sup-w1/mcp.json"),
        workdir: Some("/work/sup-w1"),
    }
}

#[test]
fn a_filled_template_is_one_element_with_the_values_in_place() {
    let v = vars(Some("opus"));
    assert_eq!(
        fill("--model={model}", &COMMAND_VARS, &v).unwrap(),
        "--model=opus"
    );
    assert_eq!(
        fill("{workdir}/{worker}.log", &COMMAND_VARS, &v).unwrap(),
        "/work/sup-w1/sup-w1.log"
    );
    assert!(fill("{model}", &COMMAND_VARS, &vars(None)).is_err());
    assert!(fill("{title}", &COMMAND_VARS, &v).is_err());
}

#[test]
fn a_value_with_a_brace_is_refused_so_it_is_never_read_as_a_placeholder() {
    let v = Vars {
        model: Some("{worker}"),
        ..vars(None)
    };
    assert!(fill("{model}", &COMMAND_VARS, &v).is_err());
}

#[test]
fn the_mcp_config_names_one_server_in_either_format() {
    let env = BTreeMap::from([
        ("KBL_AGENT".to_owned(), "sup-w1".to_owned()),
        ("KBL_TRACKER_TOKEN".to_owned(), "kblw_x\"y".to_owned()),
    ]);
    let j = mcp_config(Format::Json, Path::new("/bin/kbl-mcp"), &env).unwrap();
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v["mcpServers"]["kbl"]["command"], "/bin/kbl-mcp");
    assert_eq!(
        v["mcpServers"]["kbl"]["env"]["KBL_TRACKER_TOKEN"],
        "kblw_x\"y"
    );
    let t = mcp_config(Format::Toml, Path::new("/bin/kbl-mcp"), &env).unwrap();
    let v: toml::Value = toml::from_str(&t).unwrap();
    assert_eq!(
        v["mcp_servers"]["kbl"]["env"]["KBL_AGENT"].as_str(),
        Some("sup-w1")
    );
    assert_eq!(
        v["mcp_servers"]["kbl"]["env"]["KBL_TRACKER_TOKEN"].as_str(),
        Some("kblw_x\"y")
    );
}

#[test]
fn a_private_file_is_mode_600_and_never_written_through_an_existing_path() {
    let dir = std::env::temp_dir().join(format!("kbl-render-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("a/mcp.json");
    write_private(&file, "secret").unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let parent = std::fs::metadata(dir.join("a"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(parent, 0o700);
    assert!(write_private(&file, "again").is_err());
    let link = dir.join("a/link.json");
    std::os::unix::fs::symlink(dir.join("a/elsewhere"), &link).unwrap();
    assert!(write_private(&link, "secret").is_err());
    assert!(!dir.join("a/elsewhere").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
