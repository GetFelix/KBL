use super::*;

#[test]
fn lists_the_run_info_tool_with_a_description() {
    let tools = Server::tool_router().list_all();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, ["run_info"]);
    assert!(tools[0].description.is_some());
}
