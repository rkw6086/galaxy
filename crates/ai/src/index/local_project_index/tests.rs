use std::fs;

use tempfile::TempDir;
use warp_search_core::searcher::SimpleFullTextSearcher;

use super::*;

#[test]
fn local_index_documents_include_symbols_paths_and_body_text() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "/// Authenticate a request\npub fn authenticate_request() {}\n",
    )
    .unwrap();

    let documents = futures::executor::block_on(build_documents(root.path())).unwrap();
    assert_eq!(documents.len(), 1);
    assert!(documents[0].file_path.ends_with("src/lib.rs"));
    assert!(documents[0].symbol.contains("authenticate_request"));
    assert!(documents[0].body.contains("Authenticate a request"));
}

#[test]
fn persistent_index_can_be_reopened_without_network_access() {
    let root = TempDir::new().unwrap();
    let storage = TempDir::new().unwrap();
    fs::write(
        root.path().join("main.rs"),
        "fn local_authentication() {}\n",
    )
    .unwrap();

    let canonical_root = dunce::canonicalize(root.path()).unwrap();
    let (root_path, built, _) = futures::executor::block_on(build_persisted_index(
        canonical_root,
        storage.path().to_path_buf(),
    ))
    .unwrap();
    assert_eq!(
        built
            .searcher
            .search_id("local_authentication")
            .unwrap()
            .len(),
        1
    );

    let directory = repository_storage_directory(storage.path(), &root_path);
    publish_generation(storage.path(), &root_path, &built.generation).unwrap();
    let metadata = read_metadata(&directory).unwrap();
    let reopened = SimpleFullTextSearcher::open_in_dir(
        &LOCAL_PROJECT_INDEX_SCHEMA,
        DEFAULT_MEMORY_BUDGET,
        &directory.join("generations").join(metadata.generation),
    )
    .unwrap();
    assert_eq!(reopened.search_id("local_authentication").unwrap().len(), 1);
}

#[test]
fn partial_paths_filter_local_search_results() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::create_dir(root.path().join("tests")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "fn authentication() {}\n").unwrap();
    fs::write(root.path().join("tests/lib.rs"), "fn authentication() {}\n").unwrap();

    let storage = TempDir::new().unwrap();
    let canonical_root = dunce::canonicalize(root.path()).unwrap();
    let (_, built, file_count) = futures::executor::block_on(build_persisted_index(
        canonical_root.clone(),
        storage.path().to_path_buf(),
    ))
    .unwrap();
    let manager = LocalProjectIndex {
        root_path: canonical_root,
        searcher: built.searcher,
        file_count,
    };

    let matches = manager
        .search("authentication", Some(&["src".to_string()]))
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0].path.ends_with("src/lib.rs"));
}

#[test]
fn incremental_commit_replaces_and_removes_files_without_reparsing_unchanged_files() {
    let root = TempDir::new().unwrap();
    let storage = TempDir::new().unwrap();
    let root = dunce::canonicalize(root.path()).unwrap();
    fs::write(root.join("changed.rs"), "fn old_symbol() {}\n").unwrap();
    fs::write(root.join("untouched.rs"), "fn retained_symbol() {}\n").unwrap();
    fs::write(root.join("removed.rs"), "fn removed_symbol() {}\n").unwrap();
    let (_, initial, _) = futures::executor::block_on(build_persisted_index(
        root.clone(),
        storage.path().to_path_buf(),
    ))
    .unwrap();
    publish_generation(storage.path(), &root, &initial.generation).unwrap();
    // Keep the old reader open, as UI searches do during an update.
    drop(initial.repository_lock);
    fs::write(root.join("changed.rs"), "fn replacement_symbol() {}\n").unwrap();
    fs::write(root.join("untouched.rs"), "fn must_not_be_read() {}\n").unwrap();
    fs::remove_file(root.join("removed.rs")).unwrap();
    fs::write(root.join("added.rs"), "fn added_symbol() {}\n").unwrap();
    let paths = ["changed.rs", "removed.rs", "added.rs"]
        .map(|path| root.join(path))
        .into_iter()
        .collect();
    let (_, updated, count) = futures::executor::block_on(update_persisted_index(
        root.clone(),
        storage.path().to_path_buf(),
        paths,
    ))
    .unwrap();
    assert_eq!(count, 3);
    assert_eq!(updated.generation, initial.generation);
    assert_eq!(updated.searcher.search_id("old_symbol").unwrap().len(), 0);
    assert_eq!(
        updated.searcher.search_id("removed_symbol").unwrap().len(),
        0
    );
    for symbol in ["replacement_symbol", "retained_symbol", "added_symbol"] {
        assert_eq!(updated.searcher.search_id(symbol).unwrap().len(), 1);
    }
    assert_eq!(
        updated
            .searcher
            .search_id("must_not_be_read")
            .unwrap()
            .len(),
        0
    );
    assert_eq!(initial.searcher.search_id("old_symbol").unwrap().len(), 1);
    // A second writer can open while both readers are still alive; persisted contents survive reopen.
    drop(updated.repository_lock);
    let (_, reopened, count) = futures::executor::block_on(update_persisted_index(
        root.clone(),
        storage.path().to_path_buf(),
        HashSet::from([root.join("added.rs")]),
    ))
    .unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        reopened
            .searcher
            .search_id("replacement_symbol")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn indexed_body_is_capped_at_a_utf8_boundary_and_oversized_files_are_skipped() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("unicode.txt");
    let content = "€".repeat(MAX_INDEXED_BODY_BYTES);
    fs::write(&path, &content).unwrap();
    let document = build_document(&path).unwrap().unwrap();
    assert_eq!(
        document.body.len(),
        MAX_INDEXED_BODY_BYTES - MAX_INDEXED_BODY_BYTES % 3
    );
    assert!(content.starts_with(&document.body));
    let oversized = root.path().join("oversized.rs");
    File::create(&oversized)
        .unwrap()
        .set_len(MAX_INDEXED_FILE_BYTES as u64 + 1)
        .unwrap();
    assert!(build_document(&oversized).unwrap().is_none());
    let binary = root.path().join("binary.dat");
    fs::write(&binary, [255, 254]).unwrap();
    assert!(build_document(&binary).unwrap().is_none());
}

#[test]
fn subtree_updates_respect_inherited_ignores_and_remove_renamed_paths() {
    let root = TempDir::new().unwrap();
    let storage = TempDir::new().unwrap();
    let root = dunce::canonicalize(root.path()).unwrap();
    fs::create_dir(root.join("old")).unwrap();
    fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(root.join("old/keep.rs"), "fn kept_symbol() {}\n").unwrap();
    fs::write(root.join("old/ignored.rs"), "fn excluded_symbol() {}\n").unwrap();
    let (_, initial, _) = futures::executor::block_on(build_persisted_index(
        root.clone(),
        storage.path().to_path_buf(),
    ))
    .unwrap();
    publish_generation(storage.path(), &root, &initial.generation).unwrap();
    drop(initial);
    fs::rename(root.join("old"), root.join("new")).unwrap();
    let paths = HashSet::from([root.join("old"), root.join("new"), root.join("new/keep.rs")]);
    let (_, updated, _) = futures::executor::block_on(update_persisted_index(
        root.clone(),
        storage.path().to_path_buf(),
        paths,
    ))
    .unwrap();
    let hits = updated.searcher.search_id("kept_symbol").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        Path::new(&hits[0].values.file_path),
        root.join("new/keep.rs")
    );
    assert_eq!(
        updated.searcher.search_id("excluded_symbol").unwrap().len(),
        0
    );
}
