use std::sync::{Arc, Barrier};
use std::{fs, io};

use super::{remove_created_file, write_new_file};

#[test]
fn guarded_undo_removes_only_unchanged_created_content() {
    let temporary = tempfile::tempdir().unwrap();
    let scope = fs::canonicalize(temporary.path()).unwrap();
    let path = scope.join(".agents/skills/check/SKILL.md");
    write_new_file(&path, &scope, b"created skill").unwrap();
    remove_created_file(&path, &scope, b"created skill").unwrap();
    assert!(!path.exists());

    write_new_file(&path, &scope, b"created skill").unwrap();
    fs::write(&path, b"external edit").unwrap();
    let error = remove_created_file(&path, &scope, b"created skill").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(path).unwrap(), b"external edit");
}

#[test]
fn guarded_undo_does_not_create_missing_parent_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let scope = fs::canonicalize(temporary.path()).unwrap();
    assert!(remove_created_file(
        &scope.join(".agents/skills/check/SKILL.md"),
        &scope,
        b"created skill"
    )
    .is_err());
    assert!(!scope.join(".agents").exists());
}

#[cfg(unix)]
#[test]
fn guarded_undo_preserves_redirected_parent_and_leaf_targets() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let scope = base.join("project");
    let path = scope.join(".agents/skills/check/SKILL.md");
    fs::create_dir(&scope).unwrap();
    write_new_file(&path, &scope, b"created skill").unwrap();
    fs::rename(scope.join(".agents"), base.join("moved-agents")).unwrap();
    symlink(base.join("moved-agents"), scope.join(".agents")).unwrap();
    assert!(remove_created_file(&path, &scope, b"created skill").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"created skill");

    fs::remove_file(scope.join(".agents")).unwrap();
    fs::rename(base.join("moved-agents"), scope.join(".agents")).unwrap();
    fs::rename(&path, base.join("moved-skill.md")).unwrap();
    symlink(base.join("moved-skill.md"), &path).unwrap();
    assert!(remove_created_file(&path, &scope, b"created skill").is_err());
    assert_eq!(
        fs::read(base.join("moved-skill.md")).unwrap(),
        b"created skill"
    );
}

#[test]
fn creates_missing_skill_directories_without_replacing_an_existing_skill() {
    let temporary = tempfile::tempdir().unwrap();
    let scope = fs::canonicalize(temporary.path()).unwrap();
    let path = scope.join(".agents/skills/check/SKILL.md");

    write_new_file(&path, &scope, b"original").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"original");

    let error = write_new_file(&path, &scope, b"replacement").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path).unwrap(), b"original");
}

#[test]
fn rejects_outside_and_parent_traversal_before_creating_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let scope = base.join("project");
    fs::create_dir(&scope).unwrap();

    for path in [
        base.join("outside/.agents/skills/check/SKILL.md"),
        scope.join("../outside/.agents/skills/check/SKILL.md"),
    ] {
        let error = write_new_file(&path, &scope, b"skill").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
    assert!(!base.join("outside").exists());
}

#[test]
fn concurrent_creators_cannot_overwrite_each_other() {
    let temporary = tempfile::tempdir().unwrap();
    let scope = Arc::new(fs::canonicalize(temporary.path()).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let creators = [b"first".as_slice(), b"second".as_slice()].map(|content| {
        let scope = Arc::clone(&scope);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            (
                content,
                write_new_file(
                    &scope.join(".agents/skills/check/SKILL.md"),
                    &scope,
                    content,
                ),
            )
        })
    });
    barrier.wait();
    let outcomes = creators.map(|creator| creator.join().unwrap());
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let expected = outcomes
        .iter()
        .find(|(_, result)| result.is_ok())
        .unwrap()
        .0;
    assert_eq!(
        fs::read(scope.join(".agents/skills/check/SKILL.md")).unwrap(),
        expected
    );
    assert_eq!(
        outcomes
            .into_iter()
            .find_map(|(_, result)| result.err())
            .unwrap()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_scope_replaced_by_a_symlink_after_preparation() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let scope = base.join("project");
    let outside = base.join("outside");
    fs::create_dir(&scope).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::rename(&scope, base.join("original-project")).unwrap();
    symlink(&outside, &scope).unwrap();

    let error = write_new_file(
        &scope.join(".agents/skills/check/SKILL.md"),
        &scope,
        b"skill",
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!outside.join(".agents").exists());
}

#[cfg(unix)]
#[test]
fn rejects_changed_parent_directories_and_leaf_symlinks() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let scope = base.join("project");
    let outside = base.join("outside");
    fs::create_dir(&scope).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, scope.join(".agents")).unwrap();

    assert!(write_new_file(
        &scope.join(".agents/skills/check/SKILL.md"),
        &scope,
        b"skill"
    )
    .is_err());
    assert!(!outside.join("skills").exists());

    fs::remove_file(scope.join(".agents")).unwrap();
    let parent = scope.join(".agents/skills/check");
    fs::create_dir_all(&parent).unwrap();
    let original = outside.join("original.md");
    fs::write(&original, b"original").unwrap();
    symlink(&original, parent.join("SKILL.md")).unwrap();
    let error = write_new_file(&parent.join("SKILL.md"), &scope, b"replacement").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(original).unwrap(), b"original");
}
