use super::*;

fn context(root: &Path) -> SkillCreationContext {
    SkillCreationContext {
        project_root: Some(std::fs::canonicalize(root).unwrap()),
        home_directory: Some(std::fs::canonicalize(root.join("home")).unwrap()),
        local: true,
    }
}

fn request(scope: &str) -> ToolCall {
    ToolCall {
        id: "skill-call".to_owned(),
        name: "create_skill".to_owned(),
        arguments: serde_json::json!({
            "name": "run-channel-verifier",
            "description": "Verify channels: use for \"channel\" checks.",
            "instructions": "# Channel verification\n\nRun the project verifier and report the result.",
            "scope": scope,
        }),
    }
}

#[test]
fn project_and_global_scopes_generate_distinct_reviewable_create_actions() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("app/server")).unwrap();
    std::fs::create_dir(temp.path().join("home")).unwrap();
    let context = context(temp.path());
    let mut project = request("project");
    project.arguments["project_directory"] = serde_json::json!("app/server");
    let (lowered, scope) = context
        .lower_call(&project, &SkillPathOrigin::Local, true)
        .unwrap();
    assert_eq!(scope, context.project_root.clone().unwrap());
    assert_eq!(lowered.name, "apply_file_diffs");
    let file = &lowered.arguments["new_files"][0];
    let path = Path::new(file["file_path"].as_str().unwrap());
    assert_eq!(
        path,
        scope.join("app/server/.agents/skills/run-channel-verifier/SKILL.md")
    );
    let parsed = ai::skills::parse_skill_content_at_location(
        galaxy_util::local_or_remote_path::LocalOrRemotePath::Local(path.to_path_buf()),
        file["content"].as_str().unwrap(),
        ai::skills::SkillProvider::Agents,
        ai::skills::SkillScope::Project,
    )
    .unwrap();
    assert_eq!(parsed.name, "run-channel-verifier");
    assert_eq!(
        parsed.description,
        project.arguments["description"].as_str().unwrap()
    );
    let (global, global_scope) = context
        .lower_call(&request("global"), &SkillPathOrigin::Local, true)
        .unwrap();
    assert_eq!(global_scope, scope.join("home"));
    assert_eq!(
        Path::new(
            global.arguments["new_files"][0]["file_path"]
                .as_str()
                .unwrap()
        ),
        global_scope.join(".agents/skills/run-channel-verifier/SKILL.md")
    );
    project.arguments["project_directory"] = serde_json::json!("app/./server");
    let (live, _) = context
        .lower_call(&project, &SkillPathOrigin::Local, true)
        .unwrap();
    let (historical, _) = context
        .lower_call(&project, &SkillPathOrigin::RestoredDisplayOnly, false)
        .unwrap();
    assert_eq!(live.arguments, historical.arguments);
}

#[test]
fn existing_skills_fail_live_creation_but_remain_displayable_in_history() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("home")).unwrap();
    let context = context(temp.path());
    let path = temp
        .path()
        .join(".agents/skills/run-channel-verifier/SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "existing instructions").unwrap();
    assert!(context
        .lower_call(&request("project"), &SkillPathOrigin::Local, true)
        .unwrap_err()
        .contains("already exists"));
    assert!(context
        .lower_call(
            &request("project"),
            &SkillPathOrigin::RestoredDisplayOnly,
            false
        )
        .is_ok());
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "existing instructions"
    );
}

#[test]
fn malformed_names_scopes_and_traversal_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("home")).unwrap();
    let context = context(temp.path());
    for name in ["../escape", "a/b", "Bad Name", "-name", "name-", ""] {
        let mut call = request("project");
        call.arguments["name"] = serde_json::json!(name);
        assert!(context
            .lower_call(&call, &SkillPathOrigin::Local, true)
            .is_err());
    }
    let mut call = request("project");
    call.arguments["project_directory"] = serde_json::json!("../other-project");
    assert!(context
        .lower_call(&call, &SkillPathOrigin::Local, true)
        .is_err());
    assert!(context
        .lower_call(&request("elsewhere"), &SkillPathOrigin::Local, true)
        .is_err());
    assert!(SkillCreationContext::default()
        .lower_call(&request("global"), &SkillPathOrigin::Local, true)
        .is_err());
}

#[cfg(unix)]
#[test]
fn skill_paths_cannot_escape_scope_through_symlinks() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("home")).unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join(".agents")).unwrap();
    let context = context(project.path());
    assert!(context
        .lower_call(&request("project"), &SkillPathOrigin::Local, true)
        .unwrap_err()
        .contains("symlink"));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn provider_parent_symlinks_inside_scope_do_not_change_live_history_identity() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("home")).unwrap();
    std::fs::create_dir(project.path().join(".agents")).unwrap();
    let redirected = project.path().join("shared-skills");
    std::fs::create_dir(&redirected).unwrap();
    std::os::unix::fs::symlink(&redirected, project.path().join(".agents/skills")).unwrap();
    let context = context(project.path());

    let error = context
        .lower_call(&request("project"), &SkillPathOrigin::Local, true)
        .unwrap_err();
    assert!(error.contains("must not redirect through a symlink"));
    assert_eq!(std::fs::read_dir(redirected).unwrap().count(), 0);
}
