//! Persistent local-only structural and lexical project search.
//!
//! This index stores only bounded search metadata. Search callers read the current file contents
//! after applying their normal permission checks; no source is sent to a server by this module.

#![cfg(all(feature = "local_fs", not(target_family = "wasm")))]

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use galaxy_core::paths::state_dir;
use galaxy_util::standardized_path::StandardizedPath;
use galaxyui_core::r#async::Timer;
use galaxyui_core::{Entity, ModelContext, SingletonEntity};
use itertools::Itertools;
use repo_metadata::{RepoMetadataEvent, RepositoryIdentifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use warp_search_core::define_search_schema;
use warp_search_core::searcher::{SimpleFullTextSearcher, DEFAULT_MEMORY_BUDGET};

use crate::index::build_outline;

const INDEX_SCHEMA_VERSION: u32 = 1;
const MAX_INDEX_FILES: usize = 5_000;
const MAX_INDEXED_FILE_BYTES: usize = 3 * 1_000_000;
const MAX_INDEXED_BODY_BYTES: usize = 256_000;
const INDEX_DIRECTORY_NAME: &str = "local_project_indices";
const CURRENT_FILE_NAME: &str = "CURRENT";
const METADATA_FILE_NAME: &str = "metadata.json";
const INDEX_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

// Field weights intentionally prioritize symbols and paths over implementation text.
define_search_schema!(
    schema_name: LOCAL_PROJECT_INDEX_SCHEMA,
    config_name: LocalProjectIndexSchema,
    search_doc: LocalProjectSearchDocument,
    identifying_doc: LocalProjectIndexId,
    search_fields: [symbol: 8.0, path: 5.0, metadata: 4.0, body: 1.0],
    id_fields: [file_path: String]
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalIndexStatus {
    Indexing,
    Ready { file_count: usize },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSearchHit {
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum LocalProjectIndexEvent {
    StatusChanged {
        root_path: PathBuf,
        status: LocalIndexStatus,
    },
    IndexRemoved {
        root_path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalIndexMetadata {
    root_path: String,
    schema_version: u32,
    generation: String,
    file_count: usize,
}

struct LocalProjectIndex {
    root_path: PathBuf,
    searcher: SimpleFullTextSearcher<LocalProjectIndexSchema>,
    file_count: usize,
}

impl LocalProjectIndex {
    fn search(
        &self,
        query: &str,
        partial_path_segments: Option<&[String]>,
    ) -> Result<Vec<LocalSearchHit>> {
        let matches = self.searcher.search_id(query)?;

        Ok(matches
            .into_iter()
            .filter_map(|matched| {
                let path = PathBuf::from(matched.values.file_path);
                let relative_path = path.strip_prefix(&self.root_path).ok()?;
                if partial_path_segments.is_some_and(|segments| {
                    !segments.is_empty()
                        && !segments
                            .iter()
                            .any(|segment| relative_path.to_string_lossy().contains(segment))
                }) {
                    return None;
                }
                Some(LocalSearchHit { path })
            })
            .unique_by(|hit| hit.path.clone())
            .collect())
    }
}

pub struct LocalProjectIndexManager {
    indices: HashMap<PathBuf, LocalProjectIndex>,
    statuses: HashMap<PathBuf, LocalIndexStatus>,
    pending_rebuilds: HashSet<PathBuf>,
    rebuild_epochs: HashMap<PathBuf, u64>,
    storage_root: PathBuf,
}

impl Entity for LocalProjectIndexManager {
    type Event = LocalProjectIndexEvent;
}

impl SingletonEntity for LocalProjectIndexManager {}

impl LocalProjectIndexManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let repo_metadata = repo_metadata::RepoMetadataModel::handle(ctx);
        let manager = Self::new_at(state_dir().join(INDEX_DIRECTORY_NAME), ctx);
        ctx.subscribe_to_model(&repo_metadata, |manager, _, event, ctx| {
            manager.handle_repo_metadata_event(event, ctx);
        });

        // Re-register restored roots with repository metadata so filesystem updates continue to
        // refresh the local index after an app restart. RepositoryUpdated is intentionally ignored
        // below; the restored generation is already usable and only file-tree updates need a
        // rebuild.
        let restored_roots: Vec<PathBuf> = manager.statuses.keys().cloned().collect();
        for root_path in restored_roots {
            let Ok(standardized_root) = StandardizedPath::from_local_canonicalized(&root_path)
            else {
                continue;
            };
            let _ = repo_metadata.update(ctx, |model, ctx| {
                model.index_local_directory_path(&standardized_root, ctx)
            });
        }
        manager
    }

    /// Constructs a manager with an explicit storage root for hermetic tests.
    pub fn new_at(storage_root: PathBuf, _ctx: &mut ModelContext<Self>) -> Self {
        let mut manager = Self {
            indices: HashMap::new(),
            statuses: HashMap::new(),
            pending_rebuilds: HashSet::new(),
            rebuild_epochs: HashMap::new(),
            storage_root,
        };
        manager.restore_persisted_indices();
        manager
    }

    /// Starts a background rebuild for an explicit local project root.
    pub fn index_directory(
        &mut self,
        root_path: PathBuf,
        ctx: &mut ModelContext<Self>,
    ) -> Result<()> {
        let root_path = dunce::canonicalize(&root_path).with_context(|| {
            format!(
                "Failed to canonicalize project root {}",
                root_path.display()
            )
        })?;
        if !root_path.is_dir() {
            anyhow::bail!("Project root is not a directory: {}", root_path.display());
        }
        if matches!(
            self.statuses.get(&root_path),
            Some(LocalIndexStatus::Indexing)
        ) {
            self.pending_rebuilds.insert(root_path);
            return Ok(());
        }

        let standardized_root = StandardizedPath::from_local_canonicalized(&root_path)
            .map_err(|error| anyhow::anyhow!("Failed to standardize project root: {error}"))?;
        repo_metadata::RepoMetadataModel::handle(ctx)
            .update(ctx, |model, ctx| {
                model.index_local_directory_path(&standardized_root, ctx)
            })
            .map_err(|error| anyhow::anyhow!("Failed to register project root: {error}"))?;

        self.start_rebuild(root_path, ctx);
        Ok(())
    }

    fn start_rebuild(&mut self, root_path: PathBuf, ctx: &mut ModelContext<Self>) {
        let rebuild_epoch = {
            let epoch = self.rebuild_epochs.entry(root_path.clone()).or_default();
            *epoch += 1;
            *epoch
        };
        self.statuses
            .insert(root_path.clone(), LocalIndexStatus::Indexing);
        ctx.emit(LocalProjectIndexEvent::StatusChanged {
            root_path: root_path.clone(),
            status: LocalIndexStatus::Indexing,
        });

        let storage_root = self.storage_root.clone();
        ctx.spawn(
            async move { build_persisted_index(root_path, storage_root).await },
            move |manager, result, ctx| match result {
                Ok((root_path, built_index, file_count)) => {
                    if manager.rebuild_epochs.get(&root_path).copied() != Some(rebuild_epoch) {
                        drop(built_index.searcher);
                        let _ = fs::remove_dir_all(&built_index.generation_directory);
                        return;
                    }
                    if let Err(error) = publish_generation(
                        &manager.storage_root,
                        &root_path,
                        &built_index.generation,
                    ) {
                        drop(built_index.searcher);
                        manager.handle_rebuild_failure(root_path, error, ctx);
                        return;
                    }
                    let status = LocalIndexStatus::Ready { file_count };
                    manager.indices.insert(
                        root_path.clone(),
                        LocalProjectIndex {
                            root_path: root_path.clone(),
                            searcher: built_index.searcher,
                            file_count,
                        },
                    );
                    manager.statuses.insert(root_path.clone(), status.clone());
                    ctx.emit(LocalProjectIndexEvent::StatusChanged {
                        root_path: root_path.clone(),
                        status,
                    });
                    let should_rebuild = manager.pending_rebuilds.remove(&root_path);
                    cleanup_old_generations(&manager.storage_root, &root_path);
                    drop(built_index.repository_lock);
                    if should_rebuild {
                        manager.start_rebuild(root_path, ctx);
                    }
                }
                Err((root_path, error)) => {
                    if manager.rebuild_epochs.get(&root_path).copied() != Some(rebuild_epoch) {
                        return;
                    }
                    manager.handle_rebuild_failure(root_path, error, ctx);
                }
            },
        );
    }

    fn handle_rebuild_failure(
        &mut self,
        root_path: PathBuf,
        error: anyhow::Error,
        ctx: &mut ModelContext<Self>,
    ) {
        log::warn!(
            "Failed to refresh local project index for {}: {error:#}",
            root_path.display()
        );
        // Keep a previously committed generation searchable. Only a root with no
        // usable generation transitions to Failed.
        if let Some(previous_index) = self.indices.get(&root_path) {
            let status = LocalIndexStatus::Ready {
                file_count: previous_index.file_count,
            };
            self.statuses.insert(root_path.clone(), status.clone());
            ctx.emit(LocalProjectIndexEvent::StatusChanged {
                root_path: root_path.clone(),
                status,
            });
        } else {
            let status = LocalIndexStatus::Failed {
                message: error.to_string(),
            };
            self.statuses.insert(root_path.clone(), status.clone());
            ctx.emit(LocalProjectIndexEvent::StatusChanged {
                root_path: root_path.clone(),
                status,
            });
        }
        let should_rebuild = self.pending_rebuilds.remove(&root_path);
        if should_rebuild {
            self.start_rebuild(root_path, ctx);
        }
    }

    /// Returns the most specific indexed root containing `path`.
    pub fn status_for_path(&self, path: &Path) -> Option<(&Path, &LocalIndexStatus)> {
        let canonical_path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.statuses
            .iter()
            .filter(|(root, _)| canonical_path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(root, status)| (root.as_path(), status))
    }

    pub fn is_ready_for_path(&self, path: &Path) -> bool {
        self.status_for_path(path)
            .is_some_and(|(_, status)| matches!(status, LocalIndexStatus::Ready { .. }))
    }

    pub fn is_searchable_for_path(&self, path: &Path) -> bool {
        self.status_for_path(path).is_some_and(|(root, status)| {
            matches!(
                status,
                LocalIndexStatus::Ready { .. } | LocalIndexStatus::Indexing
            ) && self.indices.contains_key(root)
        })
    }

    pub fn root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.status_for_path(path)
            .map(|(root, _)| root.to_path_buf())
    }

    pub fn search(
        &self,
        path: &Path,
        query: &str,
        partial_path_segments: Option<&[String]>,
    ) -> Result<Vec<LocalSearchHit>> {
        let Some((root, status)) = self.status_for_path(path) else {
            anyhow::bail!("No local project index is available for {}", path.display());
        };
        // A previous generation remains searchable while a refresh is being built.
        if !matches!(
            status,
            LocalIndexStatus::Ready { .. } | LocalIndexStatus::Indexing
        ) {
            anyhow::bail!("Local project index is not ready for {}", root.display());
        }
        self.indices
            .get(root)
            .context("Local project index status has no loaded generation")?
            .search(query, partial_path_segments)
    }

    pub fn statuses(&self) -> impl Iterator<Item = (&PathBuf, &LocalIndexStatus)> {
        self.statuses.iter()
    }

    /// Removes a local project index and its persisted generations.
    pub fn remove_index_for_path(&mut self, root_path: PathBuf, ctx: &mut ModelContext<Self>) {
        let root_path = dunce::canonicalize(&root_path).unwrap_or(root_path);
        self.remove_index(&root_path, ctx);
        ctx.emit(LocalProjectIndexEvent::IndexRemoved { root_path });
    }

    fn handle_repo_metadata_event(
        &mut self,
        event: &RepoMetadataEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let local_root = match event {
            RepoMetadataEvent::FileTreeEntryUpdated {
                id: RepositoryIdentifier::Local(path),
                ..
            } => path.to_local_path(),
            // FileTreeUpdated is emitted before repository mutations are applied. The
            // post-application FileTreeEntryUpdated event below is the rebuild trigger; using
            // both would start duplicate full rebuilds for every watcher batch.
            RepoMetadataEvent::FileTreeUpdated { .. } => None,
            RepoMetadataEvent::RepositoryRemoved {
                id: RepositoryIdentifier::Local(path),
            } => {
                let local_path = path.to_local_path_lossy();
                let root_path = dunce::canonicalize(&local_path).unwrap_or(local_path);
                if self.statuses.contains_key(&root_path) {
                    self.remove_index(&root_path, ctx);
                    ctx.emit(LocalProjectIndexEvent::IndexRemoved { root_path });
                }
                return;
            }
            RepoMetadataEvent::RepositoryUpdated { .. }
            | RepoMetadataEvent::RepositoryRemoved {
                id: RepositoryIdentifier::Remote(_),
            }
            | RepoMetadataEvent::FileTreeEntryUpdated {
                id: RepositoryIdentifier::Remote(_),
                ..
            }
            | RepoMetadataEvent::UpdatingRepositoryFailed { .. }
            | RepoMetadataEvent::StandingQueryResultsUpdated { .. }
            | RepoMetadataEvent::IncrementalUpdateReady { .. } => None,
        };

        let Some(local_root) = local_root else {
            return;
        };
        let canonical_root = dunce::canonicalize(&local_root).unwrap_or(local_root);
        if self.statuses.contains_key(&canonical_root) {
            if matches!(
                self.statuses.get(&canonical_root),
                Some(LocalIndexStatus::Indexing)
            ) {
                self.pending_rebuilds.insert(canonical_root);
            } else {
                self.start_rebuild(canonical_root, ctx);
            }
        }
    }

    fn remove_index(&mut self, root_path: &Path, ctx: &mut ModelContext<Self>) {
        let root_path = dunce::canonicalize(root_path).unwrap_or_else(|_| root_path.to_path_buf());
        self.indices.remove(&root_path);
        self.statuses.remove(&root_path);
        self.pending_rebuilds.remove(&root_path);
        let epoch = self.rebuild_epochs.entry(root_path.clone()).or_default();
        *epoch += 1;
        let storage_root = self.storage_root.clone();
        ctx.spawn(
            async move {
                let _repository_lock = acquire_repository_lock(&storage_root, &root_path).await?;
                let directory = repository_storage_directory(&storage_root, &root_path);
                match fs::remove_dir_all(directory) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(anyhow::Error::new(error)),
                }
            },
            |_, result, _| {
                if let Err(error) = result {
                    log::warn!("Failed to remove local project index: {error:#}");
                }
            },
        );
    }

    fn restore_persisted_indices(&mut self) {
        let Ok(entries) = fs::read_dir(&self.storage_root) else {
            return;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            if !directory.is_dir() {
                continue;
            }
            let metadata = match read_metadata(&directory) {
                Ok(metadata) => metadata,
                Err(error) => {
                    log::warn!(
                        "Discarding invalid local project index at {}: {error:#}",
                        directory.display()
                    );
                    let _ = fs::remove_dir_all(&directory);
                    continue;
                }
            };
            if metadata.schema_version != INDEX_SCHEMA_VERSION {
                let _ = fs::remove_dir_all(&directory);
                continue;
            }

            let raw_root_path = PathBuf::from(&metadata.root_path);
            let Ok(root_path) = dunce::canonicalize(&raw_root_path) else {
                log::debug!(
                    "Discarding local project index for missing root {}",
                    raw_root_path.display()
                );
                let _ = fs::remove_dir_all(&directory);
                continue;
            };
            if !root_path.is_dir()
                || directory.file_name().and_then(|name| name.to_str())
                    != Some(&format_storage_directory_name(&root_path))
            {
                let _ = fs::remove_dir_all(&directory);
                continue;
            }

            let generation_path = directory.join("generations").join(&metadata.generation);
            match SimpleFullTextSearcher::open_in_dir(
                &LOCAL_PROJECT_INDEX_SCHEMA,
                DEFAULT_MEMORY_BUDGET,
                &generation_path,
            ) {
                Ok(searcher) => {
                    self.indices.insert(
                        root_path.clone(),
                        LocalProjectIndex {
                            root_path: root_path.clone(),
                            searcher,
                            file_count: metadata.file_count,
                        },
                    );
                    self.statuses.insert(
                        root_path,
                        LocalIndexStatus::Ready {
                            file_count: metadata.file_count,
                        },
                    );
                }
                Err(error) => {
                    log::warn!(
                        "Discarding unreadable local project index at {}: {error:#}",
                        generation_path.display()
                    );
                    let _ = fs::remove_dir_all(&directory);
                }
            }
        }
    }
}

struct BuiltLocalProjectIndex {
    searcher: SimpleFullTextSearcher<LocalProjectIndexSchema>,
    generation: String,
    generation_directory: PathBuf,
    repository_lock: File,
}

async fn build_persisted_index(
    root_path: PathBuf,
    storage_root: PathBuf,
) -> std::result::Result<(PathBuf, BuiltLocalProjectIndex, usize), (PathBuf, anyhow::Error)> {
    let error_root_path = root_path.clone();
    let result = async {
        // A desktop app and one or more CLI processes can share this storage root. Keep the
        // repository snapshot, generation publication, and cleanup in one exclusive section so
        // one process cannot delete another process's in-progress Tantivy generation.
        let repository_lock = acquire_repository_lock(&storage_root, &root_path).await?;
        let documents = build_documents(&root_path).await?;
        let index_directory = repository_storage_directory(&storage_root, &root_path);
        fs::create_dir_all(index_directory.join("generations"))?;
        let generation = format!("generation-{}", uuid::Uuid::new_v4());
        let temporary_directory = index_directory
            .join("generations")
            .join(format!(".tmp-{generation}"));
        let generation_directory = index_directory.join("generations").join(&generation);
        let _ = fs::remove_dir_all(&temporary_directory);
        fs::create_dir_all(&temporary_directory)?;

        let searcher = match SimpleFullTextSearcher::create_in_dir(
            &LOCAL_PROJECT_INDEX_SCHEMA,
            DEFAULT_MEMORY_BUDGET,
            &temporary_directory,
        ) {
            Ok(searcher) => searcher,
            Err(error) => {
                let _ = fs::remove_dir_all(&temporary_directory);
                return Err(error);
            }
        };
        if let Err(error) = searcher.build_index(build_search_documents(&documents)) {
            let _ = fs::remove_dir_all(&temporary_directory);
            return Err(error);
        }
        // Close the writer before moving the directory so the committed generation can be
        // reopened consistently on all supported platforms.
        drop(searcher);
        // The generation is complete and validated before it becomes visible through CURRENT.
        fs::rename(&temporary_directory, &generation_directory)?;
        let searcher = SimpleFullTextSearcher::open_in_dir(
            &LOCAL_PROJECT_INDEX_SCHEMA,
            DEFAULT_MEMORY_BUDGET,
            &generation_directory,
        )?;

        let metadata = LocalIndexMetadata {
            root_path: root_path.to_string_lossy().into_owned(),
            schema_version: INDEX_SCHEMA_VERSION,
            generation: generation.clone(),
            file_count: documents.len(),
        };
        let metadata_json = serde_json::to_vec_pretty(&metadata)?;
        // Store metadata inside the generation so an interrupted refresh cannot replace the
        // metadata belonging to the generation still referenced by CURRENT.
        fs::write(generation_directory.join(METADATA_FILE_NAME), metadata_json)?;

        Ok((
            root_path,
            BuiltLocalProjectIndex {
                searcher,
                generation,
                generation_directory,
                repository_lock,
            },
            documents.len(),
        ))
    }
    .await;
    result.map_err(|error| (error_root_path, error))
}

#[derive(Debug, Clone)]
struct LocalProjectDocument {
    file_path: String,
    symbol: String,
    metadata: String,
    body: String,
}

fn build_search_documents(
    documents: &[LocalProjectDocument],
) -> impl Iterator<Item = LocalProjectSearchDocument> + '_ {
    documents
        .iter()
        .cloned()
        .map(|document| LocalProjectSearchDocument {
            symbol: document.symbol,
            path: document.file_path.clone(),
            metadata: document.metadata,
            body: document.body,
            file_path: document.file_path,
        })
}

async fn build_documents(root_path: &Path) -> Result<Vec<LocalProjectDocument>> {
    let root_path = dunce::canonicalize(root_path).with_context(|| {
        format!(
            "Failed to canonicalize project root {}",
            root_path.display()
        )
    })?;
    let outline = build_outline(&root_path, Some(MAX_INDEX_FILES)).await?;
    let mut documents = Vec::new();
    for file in outline.to_file_symbols(None) {
        let relative_path = PathBuf::from(&file.path);
        let absolute_path = root_path.join(&relative_path);
        let Ok(bytes) = fs::read(&absolute_path) else {
            continue;
        };
        if bytes.is_empty() || bytes.len() > MAX_INDEXED_FILE_BYTES {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let body = content.chars().take(MAX_INDEXED_BODY_BYTES).collect();
        let extension = absolute_path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        let metadata = format!("{extension} {}", file.symbols);
        documents.push(LocalProjectDocument {
            file_path: absolute_path.to_string_lossy().into_owned(),
            symbol: file.symbols,
            metadata,
            body,
        });
    }
    Ok(documents)
}

fn format_storage_directory_name(root_path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root_path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn repository_storage_directory(storage_root: &Path, root_path: &Path) -> PathBuf {
    storage_root.join(format_storage_directory_name(root_path))
}

fn repository_lock_path(storage_root: &Path, root_path: &Path) -> PathBuf {
    storage_root.join(format!("{}.lock", format_storage_directory_name(root_path)))
}

async fn acquire_repository_lock(storage_root: &Path, root_path: &Path) -> Result<File> {
    fs::create_dir_all(storage_root)?;
    let lock_path = repository_lock_path(storage_root, root_path);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open local index lock {}", lock_path.display()))?;
    loop {
        if fs4::fs_std::FileExt::try_lock_exclusive(&lock_file)? {
            return Ok(lock_file);
        }
        Timer::after(INDEX_LOCK_RETRY_INTERVAL).await;
    }
}

fn publish_generation(storage_root: &Path, root_path: &Path, generation: &str) -> Result<()> {
    let index_directory = repository_storage_directory(storage_root, root_path);
    let temporary_current =
        index_directory.join(format!(".{CURRENT_FILE_NAME}.{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary_current, generation.as_bytes())?;
    fs::rename(&temporary_current, index_directory.join(CURRENT_FILE_NAME))?;
    Ok(())
}

fn cleanup_old_generations(storage_root: &Path, root_path: &Path) {
    let index_directory = repository_storage_directory(storage_root, root_path);
    let Ok(current) = fs::read_to_string(index_directory.join(CURRENT_FILE_NAME)) else {
        return;
    };
    let current = current.trim();
    let generations_directory = index_directory.join("generations");
    let Ok(entries) = fs::read_dir(&generations_directory) else {
        return;
    };
    for entry in entries.flatten() {
        let generation = entry.file_name();
        if generation.to_string_lossy() != current {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn read_metadata(directory: &Path) -> Result<LocalIndexMetadata> {
    let current = fs::read_to_string(directory.join(CURRENT_FILE_NAME))?;
    let current = current.trim();
    let mut components = Path::new(current).components();
    if current.is_empty()
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        anyhow::bail!("Invalid local index generation name");
    }
    let metadata_path = directory
        .join("generations")
        .join(current)
        .join(METADATA_FILE_NAME);
    let metadata: LocalIndexMetadata = serde_json::from_slice(&fs::read(metadata_path)?)?;
    if metadata.generation != current {
        anyhow::bail!("Local index generation pointer does not match metadata");
    }
    Ok(metadata)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
