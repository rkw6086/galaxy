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
