//! The language server: LSP over stdio.
//!
//! One main thread owns the editor state (open documents, the file system
//! view, the database host). Requests run on a thread pool, each against a
//! database snapshot; an edit cancels snapshots still computing, and their
//! requests are answered "content modified". Loading the workspace runs in
//! the background and reports progress.

mod convert;
mod editing;
mod gradle_export;
mod handlers;
mod library_sources;
mod settings;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use ide::AnalysisHost;
use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    InitializeParams, InitializeResult, OneOf, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, notification, request,
};
use project_model::ProjectModel;
use rustc_hash::FxHashMap;
use vfs::{FileId, Vfs};

use crate::load::{self, LoadedFiles};
use crate::log::{Context, Event, FileRef, Hashed, SafeToken};

pub(crate) use library_sources::cache_directory;

/// The progress token of workspace loading (one per load: a reload may
/// start while one is running).
const LOADING_TOKEN: &str = "kotlin-analyzer/loading";
const EXPORT_TOKEN: &str = "kotlin-analyzer/gradle";

/// `workspace/executeCommand`: reload the project model and re-read the
/// workspace's files.
pub const RELOAD_COMMAND: &str = "kotlin-analyzer.reloadWorkspace";

/// How a session ended. LSP: an `exit` not preceded by `shutdown` (or a
/// client that just went away) ends with exit code 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending {
    AfterShutdown,
    WithoutShutdown,
}

pub fn run() -> Result<Ending, String> {
    crate::log::enable_output();
    let (connection, io_threads) = Connection::stdio();
    let ending = run_on(&connection)?;
    drop(connection);
    io_threads.join().map_err(|error| error.to_string())?;
    Ok(ending)
}

/// Serves `connection` until the client says `exit` (or goes away).
pub(crate) fn run_on(connection: &Connection) -> Result<Ending, String> {
    let (initialize_id, initialize_params) = connection
        .initialize_start()
        .map_err(|error| error.to_string())?;
    let params: InitializeParams =
        serde_json::from_value(initialize_params).map_err(|error| error.to_string())?;
    let root = workspace_root(&params);
    let project_json = params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("projectJson"))
        .and_then(|path| path.as_str())
        .map(PathBuf::from);
    let diagnostics_config = settings::diagnostics_config(params.initialization_options.as_ref());
    let gradle_export = params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("gradleExport"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let log_level = settings::log_level(params.initialization_options.as_ref());
    crate::log::start_session(&root, log_level);
    log_session_start(&params, &root, &diagnostics_config, gradle_export);
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo {
            name: "kotlin-analyzer".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    connection
        .initialize_finish(
            initialize_id,
            serde_json::to_value(result).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let mut state = GlobalState::new(connection.sender.clone(), root, project_json);
    state.refreshes = ClientRefreshes::of(&params.capabilities);
    if watches_files(&params.capabilities) {
        state.register_file_watchers();
    }
    state.diagnostics_config = Arc::new(diagnostics_config);
    state.gradle_export = gradle_export;
    state.start_loading();
    state.start_export();
    let ending = state.main_loop(&connection.receiver);
    Event::info("session-ended")
        .text(
            "ending",
            match ending {
                Ok(Ending::AfterShutdown) => "after-shutdown",
                Ok(Ending::WithoutShutdown) => "without-shutdown",
                Err(_) => "connection-error",
            },
        )
        .count("requests", state.requests_received)
        .count("panics", crate::log::panics_so_far())
        .emit();
    ending
}

/// The session's first event: this build, the machine, the client and the
/// settings — what any report needs first.
fn log_session_start(
    params: &InitializeParams,
    root: &std::path::Path,
    diagnostics: &ide::DiagnosticsConfig,
    gradle_export: bool,
) {
    let client = params.client_info.as_ref();
    let disabled: Vec<&'static str> = diagnostics
        .disabled
        .iter()
        .map(|check| check.name())
        .collect();
    let threads = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    Event::info("session-started")
        .text("version", env!("CARGO_PKG_VERSION"))
        .text("os", std::env::consts::OS)
        .text("arch", std::env::consts::ARCH)
        .count("cpus", threads)
        .token(
            "client",
            SafeToken::of(client.map_or("unknown", |client| client.name.as_str())),
        )
        .token(
            "client-version",
            SafeToken::of(
                client
                    .and_then(|client| client.version.as_deref())
                    .unwrap_or("unknown"),
            ),
        )
        .hash("workspace", Hashed::of(&root.to_string_lossy()))
        .flag("gradle-export", gradle_export)
        .flag("experimental-checks", diagnostics.experimental)
        .texts("disabled-checks", disabled)
        .emit();
    if let Some(location) = crate::log::file_location() {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr().lock(),
            "kotlin-analyzer: this session's log is {location}"
        );
    }
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        type_definition_provider: Some(lsp_types::TypeDefinitionProviderCapability::Simple(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        completion_provider: Some(lsp_types::CompletionOptions {
            trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
            resolve_provider: Some(true),
            ..lsp_types::CompletionOptions::default()
        }),
        signature_help_provider: Some(lsp_types::SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
            retrigger_characters: Some(vec![",".to_owned()]),
            ..lsp_types::SignatureHelpOptions::default()
        }),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
        })),
        folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(
            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                lsp_types::SemanticTokensOptions {
                    legend: editing::semantic_tokens_legend(),
                    full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                    ..lsp_types::SemanticTokensOptions::default()
                },
            ),
        ),
        inlay_hint_provider: Some(OneOf::Right(
            lsp_types::InlayHintServerCapabilities::Options(lsp_types::InlayHintOptions {
                resolve_provider: Some(true),
                ..lsp_types::InlayHintOptions::default()
            }),
        )),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        execute_command_provider: Some(lsp_types::ExecuteCommandOptions {
            commands: vec![RELOAD_COMMAND.to_owned()],
            ..lsp_types::ExecuteCommandOptions::default()
        }),
        ..ServerCapabilities::default()
    }
}

/// The first workspace folder, else the (deprecated) root URI, else the
/// current directory.
fn workspace_root(params: &InitializeParams) -> PathBuf {
    let from_folders = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| convert::path_of(&folder.uri));
    #[allow(deprecated)]
    let from_root_uri = params.root_uri.as_ref().and_then(convert::path_of);
    from_folders
        .or(from_root_uri)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Work finished off the main thread.
enum Task {
    Response(Response),
    /// A load finished; `generation` tells it from a later one.
    Loaded {
        generation: u64,
        loaded: Result<LoadedFiles, LoadFailure>,
    },
    /// The background Gradle export finished, after `took`.
    ModelExported {
        export: gradle_export::Export,
        took: Duration,
    },
    Diagnostics {
        file: FileId,
        version: i32,
        /// Which computation for the file it is: only the latest asked
        /// for is published (an earlier one may finish later).
        generation: u64,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
}

/// Whether the client watches files for the server when asked to.
fn watches_files(capabilities: &lsp_types::ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .and_then(|watched| watched.dynamic_registration)
        .unwrap_or(false)
}

/// What the client can be asked to request again once the workspace has
/// loaded (tokens and hints computed before then knew less).
#[derive(Clone, Copy, Default)]
struct ClientRefreshes {
    semantic_tokens: bool,
    inlay_hints: bool,
}

impl ClientRefreshes {
    fn of(capabilities: &lsp_types::ClientCapabilities) -> ClientRefreshes {
        let workspace = capabilities.workspace.as_ref();
        ClientRefreshes {
            semantic_tokens: workspace
                .and_then(|workspace| workspace.semantic_tokens.as_ref())
                .and_then(|tokens| tokens.refresh_support)
                .unwrap_or(false),
            inlay_hints: workspace
                .and_then(|workspace| workspace.inlay_hint.as_ref())
                .and_then(|hints| hints.refresh_support)
                .unwrap_or(false),
        }
    }
}

struct GlobalState {
    sender: Sender<Message>,
    root: PathBuf,
    /// Where the exported project model is, when not in the workspace.
    project_json: Option<PathBuf>,
    host: AnalysisHost,
    vfs: Vfs,
    model: Arc<ProjectModel>,
    /// Open documents and their versions.
    open_documents: FxHashMap<FileId, i32>,
    loaded: bool,
    pool: rayon::ThreadPool,
    task_sender: Sender<Task>,
    task_receiver: Receiver<Task>,
    /// Paths by file id, shared with request threads.
    paths: Arc<Vec<PathBuf>>,
    next_request: i32,
    shutdown_requested: bool,
    /// Which checks run, from the client's settings.
    diagnostics_config: Arc<ide::DiagnosticsConfig>,
    /// Whether the server may run Gradle to export the model.
    gradle_export: bool,
    exporting: bool,
    /// The latest load started: only its result is applied.
    load_generation: u64,
    /// The latest diagnostics computation asked for, per open file.
    diagnostics_generations: FxHashMap<FileId, u64>,
    next_diagnostics_generation: u64,
    refreshes: ClientRefreshes,
    /// For the log: how many requests the client sent.
    requests_received: u64,
}

impl GlobalState {
    fn new(sender: Sender<Message>, root: PathBuf, project_json: Option<PathBuf>) -> GlobalState {
        let (task_sender, task_receiver) = crossbeam_channel::unbounded();
        let threads = std::thread::available_parallelism()
            .map(|count| count.get().min(8))
            .unwrap_or(4);
        GlobalState {
            sender,
            model: Arc::new(ProjectModel::empty(root.clone())),
            root,
            project_json,
            host: AnalysisHost::default(),
            vfs: Vfs::default(),
            open_documents: FxHashMap::default(),
            loaded: false,
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|index| format!("kotlin-analyzer-worker-{index}"))
                // A job's panic is caught (and logged) where it runs; this
                // keeps one that slips through from aborting the server.
                .panic_handler(|_| {})
                .build()
                .expect("a thread pool of a few threads can be built"),
            task_sender,
            task_receiver,
            paths: Arc::default(),
            next_request: 0,
            shutdown_requested: false,
            diagnostics_config: Arc::default(),
            gradle_export: true,
            exporting: false,
            load_generation: 0,
            diagnostics_generations: FxHashMap::default(),
            next_diagnostics_generation: 0,
            refreshes: ClientRefreshes::default(),
            requests_received: 0,
        }
    }

    fn main_loop(&mut self, receiver: &Receiver<Message>) -> Result<Ending, String> {
        loop {
            select! {
                recv(receiver) -> message => {
                    let Ok(message) = message else { return Ok(Ending::WithoutShutdown) };
                    match message {
                        Message::Request(request) => {
                            if request.method == request::Shutdown::METHOD {
                                Event::info("shutdown-requested").emit();
                                self.shutdown_requested = true;
                                self.respond(Response::new_ok(request.id, ()));
                                continue;
                            }
                            self.on_request(request);
                        }
                        Message::Notification(notification) => {
                            if notification.method == notification::Exit::METHOD {
                                return Ok(if self.shutdown_requested {
                                    Ending::AfterShutdown
                                } else {
                                    Ending::WithoutShutdown
                                });
                            }
                            self.on_notification(notification);
                        }
                        Message::Response(_) => {}
                    }
                }
                recv(self.task_receiver) -> task => {
                    let Ok(task) = task else { return Ok(Ending::WithoutShutdown) };
                    self.on_task(task);
                }
            }
        }
    }

    /// Reads the workspace from disk in the background; a load started
    /// later supersedes this one.
    fn start_loading(&mut self) {
        self.load_generation += 1;
        let generation = self.load_generation;
        let token = loading_token(generation);
        self.send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: lsp_types::NumberOrString::String(token.clone()),
        });
        self.progress_on(
            &token,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: "Loading workspace".to_owned(),
                cancellable: Some(false),
                message: Some(self.root.display().to_string()),
                percentage: None,
            }),
        );
        let root = self.root.clone();
        let project_json = self.model_file();
        Event::info("load-started")
            .count("generation", generation)
            .text("model", self.model_source(project_json.is_some()))
            .emit();
        let sender = self.task_sender.clone();
        std::thread::Builder::new()
            .name("kotlin-analyzer-loader".to_owned())
            .spawn(move || {
                // A panic (logged by the hook) still ends the load: the
                // editor isn't left waiting for it.
                let loaded = crate::log::within(Context::Loading { generation }, || {
                    std::panic::catch_unwind(|| {
                        #[cfg(test)]
                        if root.join("panic-while-loading").exists() {
                            panic!("a test panic while loading");
                        }
                        load::read_workspace(&root, project_json.as_deref())
                    })
                });
                let loaded = match loaded {
                    Ok(Ok(loaded)) => Ok(loaded),
                    Ok(Err(error)) => Err(LoadFailure::Model(error)),
                    Err(_) => Err(LoadFailure::Panicked),
                };
                let _ = sender.send(Task::Loaded { generation, loaded });
            })
            .expect("a thread can be spawned");
    }

    /// Where the model a load reads comes from, for the log.
    fn model_source(&self, has_model_file: bool) -> &'static str {
        if self.project_json.is_some() {
            "given-project-json"
        } else if self.root.join(".kotlin-analyzer/project.json").is_file() {
            "workspace-project-json"
        } else if has_model_file {
            "exported-gradle-model"
        } else {
            "directory-conventions"
        }
    }

    /// The model file to load: the one given, the workspace's own, or the
    /// one exported to the cache (when there is one).
    fn model_file(&self) -> Option<PathBuf> {
        if self.project_json.is_some() || self.root.join(".kotlin-analyzer/project.json").is_file()
        {
            return self.project_json.clone();
        }
        gradle_export::cached_model(&self.root).filter(|cached| cached.is_file())
    }

    /// Exports the Gradle model in the background when it would come from
    /// the cache and isn't current there (the build files are walked off
    /// the main thread); an export that changed it reloads.
    fn start_export(&mut self) {
        let given = self.project_json.is_some()
            || self.root.join(".kotlin-analyzer/project.json").is_file();
        if given
            || !self.gradle_export
            || self.exporting
            || !gradle_export::is_gradle_build(&self.root)
        {
            return;
        }
        let Some(cached) = gradle_export::cached_model(&self.root) else {
            return;
        };
        self.exporting = true;
        self.send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: lsp_types::NumberOrString::String(EXPORT_TOKEN.to_owned()),
        });
        self.progress_on(
            EXPORT_TOKEN,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: "Exporting the Gradle model".to_owned(),
                cancellable: Some(false),
                message: Some("gradle kotlinAnalyzerModel".to_owned()),
                percentage: None,
            }),
        );
        let root = self.root.clone();
        let sender = self.task_sender.clone();
        let tool = if root.join("gradlew").is_file() {
            "gradle-wrapper"
        } else {
            "gradle-on-path"
        };
        Event::info("gradle-export-started")
            .text("tool", tool)
            .emit();
        std::thread::Builder::new()
            .name("kotlin-analyzer-gradle".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let export = crate::log::within(Context::GradleExport, || {
                    std::panic::catch_unwind(|| gradle_export::export_if_stale(&root, &cached))
                })
                .unwrap_or_else(|_| {
                    gradle_export::Export::Failed(gradle_export::ExportFailure::panicked())
                });
                let took = started.elapsed();
                let _ = sender.send(Task::ModelExported { export, took });
            })
            .expect("a thread can be spawned");
    }

    fn on_task(&mut self, task: Task) {
        match task {
            Task::Response(response) => self.respond(response),
            Task::ModelExported { export, took } => self.on_export(export, took),
            Task::Loaded { generation, loaded } => {
                if generation != self.load_generation {
                    Event::info("load-superseded")
                        .count("generation", generation)
                        .emit();
                    // Superseded by a later load, which reports for both.
                    self.progress_on(
                        &loading_token(generation),
                        WorkDoneProgress::End(WorkDoneProgressEnd {
                            message: Some("superseded".to_owned()),
                        }),
                    );
                    return;
                }
                self.on_loaded(loaded);
            }
            Task::Diagnostics {
                file,
                version,
                generation,
                diagnostics,
            } => {
                let current = self.open_documents.get(&file).copied();
                let latest = self.diagnostics_generations.get(&file).copied();
                if current != Some(version) || latest != Some(generation) {
                    Event::debug("diagnostics-stale")
                        .file("file", self.file_ref(file))
                        .count("generation", generation)
                        .emit();
                    return;
                }
                let Some(uri) = convert::uri_of(self.vfs.path(file)) else {
                    return;
                };
                self.notify::<notification::PublishDiagnostics>(
                    lsp_types::PublishDiagnosticsParams {
                        uri,
                        diagnostics,
                        version: Some(version),
                    },
                );
            }
        }
    }

    fn on_export(&mut self, export: gradle_export::Export, took: Duration) {
        self.exporting = false;
        let message = match &export {
            gradle_export::Export::UpToDate => "up to date",
            gradle_export::Export::Exported => "exported",
            gradle_export::Export::Failed(_) => "failed",
        };
        match &export {
            gradle_export::Export::Failed(failure) => Event::warn("gradle-export-failed")
                .text("reason", failure.kind.name())
                .number("exit-code", failure.exit_code.map_or(-1, i64::from))
                .duration("took", took)
                .emit(),
            gradle_export::Export::UpToDate | gradle_export::Export::Exported => {
                Event::info("gradle-export-finished")
                    .text("outcome", message)
                    .duration("took", took)
                    .emit();
            }
        }
        self.progress_on(
            EXPORT_TOKEN,
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(message.to_owned()),
            }),
        );
        match export {
            gradle_export::Export::UpToDate => {}
            gradle_export::Export::Exported => self.start_loading(),
            gradle_export::Export::Failed(failure) => {
                self.notify::<notification::ShowMessage>(lsp_types::ShowMessageParams {
                    typ: lsp_types::MessageType::WARNING,
                    message: format!(
                        "kotlin-analyzer couldn't export the Gradle model (libraries stay unknown):\n{}",
                        failure.detail
                    ),
                })
            }
        }
    }

    fn on_loaded(&mut self, loaded: Result<LoadedFiles, LoadFailure>) {
        let token = loading_token(self.load_generation);
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                Event::warn("load-failed")
                    .count("generation", self.load_generation)
                    .text("reason", error.kind())
                    .emit();
                self.loaded = true;
                self.progress_on(
                    &token,
                    WorkDoneProgress::End(WorkDoneProgressEnd {
                        message: Some(format!("loading failed: {error}")),
                    }),
                );
                self.notify::<notification::ShowMessage>(lsp_types::ShowMessageParams {
                    typ: lsp_types::MessageType::ERROR,
                    message: format!("kotlin-analyzer: {error}"),
                });
                return;
            }
        };
        let started = Instant::now();
        let files = loaded.files.len();
        let summary = LoadSummary::of(&loaded);
        self.model = loaded.model.clone();
        let open: Vec<PathBuf> = self
            .open_documents
            .keys()
            .map(|file| self.vfs.path(*file).to_path_buf())
            .collect();
        load::apply_files(
            &mut self.host,
            &mut self.vfs,
            &loaded.model,
            loaded.files,
            |path| open.iter().any(|open_path| open_path == path),
        );
        load::apply_libraries(&mut self.host, loaded.libraries);
        self.loaded = true;
        self.refresh_paths();
        summary.emit(self.load_generation, started.elapsed());
        self.progress_on(
            &token,
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(format!("{files} files")),
            }),
        );
        self.update_diagnostics(self.open_documents.keys().copied().collect());
        self.ask_for_refreshes();
    }

    /// Asks the client to report changes to sources, build files and the
    /// model file made outside the editor (a `git checkout`, a generator).
    fn register_file_watchers(&mut self) {
        let mut globs: Vec<String> = ["**/*.kt", "**/*.java"]
            .iter()
            .map(|glob| (*glob).to_owned())
            .collect();
        globs.extend(gradle_export::build_file_globs());
        globs.push("**/.kotlin-analyzer/project.json".to_owned());
        if let Some(model) = &self.project_json {
            globs.push(model.display().to_string());
        }
        let watchers: Vec<lsp_types::FileSystemWatcher> = globs
            .into_iter()
            .map(|glob| lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::String(glob),
                kind: None,
            })
            .collect();
        let options = lsp_types::DidChangeWatchedFilesRegistrationOptions { watchers };
        self.send_request::<request::RegisterCapability>(lsp_types::RegistrationParams {
            registrations: vec![lsp_types::Registration {
                id: "kotlin-analyzer/watched-files".to_owned(),
                method: notification::DidChangeWatchedFiles::METHOD.to_owned(),
                register_options: serde_json::to_value(options).ok(),
            }],
        });
    }

    /// Files changed on disk: sources not open in the editor are read
    /// again; a build file may change the model (exported again if so), the
    /// model file itself reloads the workspace.
    fn on_watched_files(&mut self, changes: Vec<lsp_types::FileEvent>) {
        let mut model_changed = false;
        let mut build_changed = false;
        let mut sources = 0usize;
        let mut open_skipped = 0usize;
        let reported = changes.len();
        for change in changes {
            let Some(path) = convert::path_of(&change.uri) else {
                continue;
            };
            if self.is_model_file(&path) {
                model_changed = true;
                continue;
            }
            if gradle_export::is_build_file(&path) {
                build_changed = true;
                continue;
            }
            if syntax::Language::of_path(&path).is_none() {
                continue;
            }
            let is_open = self
                .vfs
                .file_id(&path)
                .is_some_and(|file| self.open_documents.contains_key(&file));
            if is_open {
                open_skipped += 1;
                continue;
            }
            sources += 1;
            let text = match change.typ {
                lsp_types::FileChangeType::DELETED => None,
                _ => std::fs::read_to_string(&path).ok().map(Arc::from),
            };
            self.vfs.set_contents(&path, text);
        }
        Event::debug("watched-files-changed")
            .count("reported", reported)
            .count("sources-read", sources)
            .count("open-skipped", open_skipped)
            .flag("build-files", build_changed)
            .flag("model-file", model_changed)
            .emit();
        self.apply_vfs_changes();
        self.update_diagnostics(self.open_documents.keys().copied().collect());
        if model_changed {
            self.start_loading();
        }
        if build_changed {
            self.start_export();
        }
    }

    fn is_model_file(&self, path: &std::path::Path) -> bool {
        let workspace_model = self.root.join(".kotlin-analyzer/project.json");
        path == workspace_model || self.project_json.as_deref() == Some(path)
    }

    /// Tokens and hints the client asked for while loading were computed
    /// without the libraries: it's asked to request them again.
    fn ask_for_refreshes(&mut self) {
        if self.refreshes.semantic_tokens {
            self.send_request::<request::SemanticTokensRefresh>(());
        }
        if self.refreshes.inlay_hints {
            self.send_request::<request::InlayHintRefreshRequest>(());
        }
    }

    fn on_request(&mut self, request: Request) {
        if self.shutdown_requested {
            self.respond(Response::new_err(
                request.id,
                lsp_server::ErrorCode::InvalidRequest as i32,
                "shutdown was requested".to_owned(),
            ));
            return;
        }
        self.requests_received += 1;
        if request.method == request::ExecuteCommand::METHOD {
            self.execute_command(request);
            return;
        }
        let request_file = self.request_file_of(&request);
        let method = known_method(&request.method);
        let id = request_number(&request.id);
        let file = request_file.map(|file| self.file_ref(file));
        log_request(&request, id, method, file);
        let snapshot = handlers::Snapshot {
            analysis: self.host.analysis(),
            paths: self.paths.clone(),
            loaded: self.loaded,
            request_file,
            diagnostics_config: self.diagnostics_config.clone(),
        };
        let sender = self.task_sender.clone();
        let received = Instant::now();
        self.pool.spawn(move || {
            let started = Instant::now();
            let request_id = request.id.clone();
            let context = Context::Request { id, method, file };
            let handled = crate::log::within(context, || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handlers::handle(snapshot, request)
                }))
            });
            let response = handled.unwrap_or_else(|_| {
                Response::new_err(
                    request_id,
                    lsp_server::ErrorCode::InternalError as i32,
                    "kotlin-analyzer failed on this request; its log says where".to_owned(),
                )
            });
            let timing = Timing {
                waited: started - received,
                took: started.elapsed(),
            };
            log_response(&response, id, method, file, timing);
            let _ = sender.send(Task::Response(response));
        });
    }

    /// A file as the log names it.
    fn file_ref(&self, file: FileId) -> FileRef {
        FileRef::new(file.index() as u32, self.vfs.path(file), &self.root)
    }

    /// `workspace/executeCommand`, on the main thread: the commands change
    /// the server's state.
    fn execute_command(&mut self, request: Request) {
        let params: Result<lsp_types::ExecuteCommandParams, _> =
            serde_json::from_value(request.params.clone());
        match params {
            Ok(params) if params.command == RELOAD_COMMAND => {
                Event::info("reload-requested").emit();
                self.respond(Response::new_ok(request.id, serde_json::Value::Null));
                self.start_loading();
                self.start_export();
            }
            _ => self.respond(Response::new_err(
                request.id,
                lsp_server::ErrorCode::InvalidParams as i32,
                format!("commands: {RELOAD_COMMAND}"),
            )),
        }
    }

    /// The file a request is about, resolved on the main thread where the
    /// file system view lives.
    fn request_file_of(&self, request: &Request) -> Option<FileId> {
        // `completionItem/resolve` carries it in the item's `data`.
        let document = request.params.get("textDocument").or_else(|| {
            request
                .params
                .get("data")
                .and_then(|data| data.get("textDocument"))
        });
        let uri: lsp_types::Uri = document
            .and_then(|document| document.get("uri"))
            .and_then(|uri| serde_json::from_value(uri.clone()).ok())?;
        let path = convert::path_of(&uri)?;
        self.vfs.file_id(&path)
    }

    fn on_notification(&mut self, notification: Notification) {
        let notification =
            match cast_notification::<notification::DidOpenTextDocument>(notification) {
                Ok(params) => {
                    if let Some(path) = convert::path_of(&params.text_document.uri) {
                        let text = params.text_document.text;
                        let shape = TextShape::of(&text);
                        let file = self.vfs.set_contents(&path, Some(Arc::from(text)));
                        self.open_documents
                            .insert(file, params.text_document.version);
                        shape
                            .describe(Event::debug("document-opened"))
                            .file("file", self.file_ref(file))
                            .number("version", i64::from(params.text_document.version))
                            .emit();
                        self.apply_vfs_changes();
                        self.update_diagnostics(vec![file]);
                    }
                    return;
                }
                Err(notification) => notification,
            };
        let notification =
            match cast_notification::<notification::DidChangeTextDocument>(notification) {
                Ok(params) => {
                    let Some(path) = convert::path_of(&params.text_document.uri) else {
                        return;
                    };
                    // Full sync: the last change carries the whole text.
                    let changes = params.content_changes.len();
                    let Some(change) = params.content_changes.into_iter().last() else {
                        return;
                    };
                    let shape = TextShape::of(&change.text);
                    let file = self.vfs.set_contents(&path, Some(Arc::from(change.text)));
                    self.open_documents
                        .insert(file, params.text_document.version);
                    shape
                        .describe(Event::debug("document-changed"))
                        .file("file", self.file_ref(file))
                        .number("version", i64::from(params.text_document.version))
                        .count("changes", changes)
                        .emit();
                    self.apply_vfs_changes();
                    // An edit can change what other files' code means: all
                    // open files are checked again, the edited one first.
                    self.update_diagnostics(self.edited_first(file));
                    return;
                }
                Err(notification) => notification,
            };
        let notification =
            match cast_notification::<notification::DidChangeWatchedFiles>(notification) {
                Ok(params) => {
                    self.on_watched_files(params.changes);
                    return;
                }
                Err(notification) => notification,
            };
        let notification =
            match cast_notification::<notification::DidChangeConfiguration>(notification) {
                Ok(params) => {
                    if let Some(level) = settings::changed_log_level(&params.settings) {
                        crate::log::set_level(level);
                    }
                    if let Some(config) = settings::changed_diagnostics_config(&params.settings) {
                        let disabled: Vec<&'static str> =
                            config.disabled.iter().map(|check| check.name()).collect();
                        Event::info("settings-changed")
                            .flag("experimental-checks", config.experimental)
                            .texts("disabled-checks", disabled)
                            .emit();
                        self.diagnostics_config = Arc::new(config);
                        self.update_diagnostics(self.open_documents.keys().copied().collect());
                    }
                    return;
                }
                Err(notification) => notification,
            };
        if let Ok(params) = cast_notification::<notification::DidCloseTextDocument>(notification) {
            let Some(path) = convert::path_of(&params.text_document.uri) else {
                return;
            };
            if let Some(file) = self.vfs.file_id(&path) {
                Event::debug("document-closed")
                    .file("file", self.file_ref(file))
                    .emit();
                self.open_documents.remove(&file);
                self.diagnostics_generations.remove(&file);
            }
            // The disk's version is the file's text again.
            let on_disk = std::fs::read_to_string(&path).ok().map(Arc::from);
            self.vfs.set_contents(&path, on_disk);
            self.apply_vfs_changes();
            self.notify::<notification::PublishDiagnostics>(lsp_types::PublishDiagnosticsParams {
                uri: params.text_document.uri,
                diagnostics: Vec::new(),
                version: None,
            });
        }
    }

    fn apply_vfs_changes(&mut self) {
        let changes = load::file_changes(&mut self.vfs, &self.model);
        if !changes.is_empty() {
            self.host.apply_change(changes);
        }
        self.refresh_paths();
    }

    fn refresh_paths(&mut self) {
        if self.paths.len() == self.vfs.len() {
            return;
        }
        self.paths = Arc::new(self.vfs.paths().to_vec());
    }

    /// The open files, `edited` first.
    fn edited_first(&self, edited: FileId) -> Vec<FileId> {
        let mut files = vec![edited];
        files.extend(
            self.open_documents
                .keys()
                .copied()
                .filter(|file| *file != edited),
        );
        files
    }

    fn update_diagnostics(&mut self, files: Vec<FileId>) {
        for file in files {
            let Some(version) = self.open_documents.get(&file).copied() else {
                continue;
            };
            self.next_diagnostics_generation += 1;
            let generation = self.next_diagnostics_generation;
            self.diagnostics_generations.insert(file, generation);
            let analysis = self.host.analysis();
            let sender = self.task_sender.clone();
            let file_ref = self.file_ref(file);
            let syntax_only = !self.loaded;
            // Until the workspace is loaded only syntax is known for sure.
            let config = if self.loaded {
                self.diagnostics_config.clone()
            } else {
                Arc::new(ide::DiagnosticsConfig {
                    only: vec![ide::Check::Syntax],
                    ..ide::DiagnosticsConfig::default()
                })
            };
            let received = Instant::now();
            self.pool.spawn(move || {
                let started = Instant::now();
                let context = Context::Diagnostics {
                    file: file_ref,
                    version,
                };
                let computed = crate::log::within(context, || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handlers::diagnostics(&analysis, file, &config)
                    }))
                });
                let timing = Timing {
                    waited: started - received,
                    took: started.elapsed(),
                };
                let outcome = match &computed {
                    Ok(Ok(_)) => "computed",
                    Ok(Err(_)) => "cancelled",
                    Err(_) => "panicked",
                };
                let slow = timing.total() > SLOW_DIAGNOSTICS;
                let event = if slow {
                    Event::warn("slow-diagnostics")
                } else {
                    Event::debug("diagnostics")
                };
                let count = match &computed {
                    Ok(Ok(diagnostics)) => diagnostics.len(),
                    _ => 0,
                };
                let event = event
                    .file("file", file_ref)
                    .number("version", i64::from(version))
                    .count("generation", generation)
                    .flag("syntax-only", syntax_only)
                    .text("outcome", outcome)
                    .count("found", count);
                timing.describe(event).emit();
                let Ok(Ok(diagnostics)) = computed else {
                    return;
                };
                let _ = sender.send(Task::Diagnostics {
                    file,
                    version,
                    generation,
                    diagnostics,
                });
            });
        }
    }

    fn respond(&self, response: Response) {
        let _ = self.sender.send(Message::Response(response));
    }

    fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        let notification = Notification::new(N::METHOD.to_owned(), params);
        let _ = self.sender.send(Message::Notification(notification));
    }

    fn send_request<R: lsp_types::request::Request>(&mut self, params: R::Params) {
        self.next_request += 1;
        let request = Request::new(
            RequestId::from(format!("kotlin-analyzer/{}", self.next_request)),
            R::METHOD.to_owned(),
            params,
        );
        let _ = self.sender.send(Message::Request(request));
    }

    fn progress_on(&self, token: &str, progress: WorkDoneProgress) {
        self.notify::<notification::Progress>(lsp_types::ProgressParams {
            token: lsp_types::NumberOrString::String(token.to_owned()),
            value: lsp_types::ProgressParamsValue::WorkDone(progress),
        });
    }
}

/// A request slower than this is logged with what led to it.
const SLOW_REQUEST: Duration = Duration::from_secs(1);
const SLOW_DIAGNOSTICS: Duration = Duration::from_secs(2);

/// The LSP methods the log names; any other is `other` (a method is the
/// client's text).
const KNOWN_METHODS: &[&str] = &[
    "textDocument/definition",
    "textDocument/declaration",
    "textDocument/typeDefinition",
    "textDocument/implementation",
    "textDocument/hover",
    "textDocument/completion",
    "completionItem/resolve",
    "textDocument/signatureHelp",
    "textDocument/references",
    "textDocument/documentHighlight",
    "textDocument/prepareRename",
    "textDocument/rename",
    "textDocument/documentSymbol",
    "workspace/symbol",
    "textDocument/foldingRange",
    "textDocument/semanticTokens/full",
    "textDocument/semanticTokens/range",
    "textDocument/inlayHint",
    "inlayHint/resolve",
    "textDocument/codeAction",
    "workspace/executeCommand",
    "shutdown",
];

fn known_method(method: &str) -> &'static str {
    KNOWN_METHODS
        .iter()
        .find(|known| **known == method)
        .copied()
        .unwrap_or("other")
}

/// A request's id when it's a number (editors number them); `-1` else.
fn request_number(id: &RequestId) -> i64 {
    serde_json::to_value(id)
        .ok()
        .and_then(|value| value.as_i64())
        .unwrap_or(-1)
}

/// A request received: its method, file and position — never its text
/// (a symbol query, a new name).
fn log_request(request: &Request, id: i64, method: &'static str, file: Option<FileRef>) {
    let mut event = Event::debug("request")
        .number("id", id)
        .text("method", method);
    if let Some(file) = file {
        event = event.file("file", file);
    }
    let position = request.params.get("position");
    let coordinate = |key: &str| {
        position
            .and_then(|position| position.get(key))
            .and_then(serde_json::Value::as_i64)
    };
    if let (Some(line), Some(column)) = (coordinate("line"), coordinate("character")) {
        event = event.number("line", line).number("column", column);
    }
    event.emit();
}

/// A request answered: how, how long it took, how much it found. A slow
/// one is a warning, written with the recorder's recent events.
fn log_response(
    response: &Response,
    id: i64,
    method: &'static str,
    file: Option<FileRef>,
    timing: Timing,
) {
    let slow = timing.total() > SLOW_REQUEST;
    let mut event = if slow {
        Event::warn("slow-request")
    } else {
        Event::debug("response")
    };
    event = event.number("id", id).text("method", method);
    if let Some(file) = file {
        event = event.file("file", file);
    }
    event = match &response.response_result {
        Err(error) if error.code == lsp_server::ErrorCode::ContentModified as i32 => {
            event.text("outcome", "cancelled")
        }
        Err(error) => event
            .text("outcome", "error")
            .number("code", i64::from(error.code)),
        Ok(result) => event.text("outcome", "ok").count("items", items_in(result)),
    };
    timing.describe(event).emit();
}

/// How long work waited for a free worker, and how long it then took: the
/// user waits for both.
#[derive(Clone, Copy)]
struct Timing {
    waited: Duration,
    took: Duration,
}

impl Timing {
    fn total(self) -> Duration {
        self.waited + self.took
    }

    fn describe(self, event: Event) -> Event {
        event
            .duration("waited", self.waited)
            .duration("took", self.took)
    }
}

/// How many things an answer holds: an array's elements, a list's items,
/// semantic tokens' tokens; one for anything else, none for `null`.
fn items_in(result: &serde_json::Value) -> usize {
    match result {
        serde_json::Value::Null => 0,
        serde_json::Value::Array(elements) => elements.len(),
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::Array(items)) = object.get("items") {
                items.len()
            } else if let Some(serde_json::Value::Array(data)) = object.get("data") {
                data.len() / 5
            } else {
                1
            }
        }
        _ => 1,
    }
}

/// A document's size, never its text.
struct TextShape {
    bytes: usize,
    lines: usize,
}

impl TextShape {
    fn of(text: &str) -> TextShape {
        TextShape {
            bytes: text.len(),
            lines: text.lines().count(),
        }
    }

    fn describe(&self, event: Event) -> Event {
        event.count("bytes", self.bytes).count("lines", self.lines)
    }
}

/// What a workspace load found, for its log event.
struct LoadSummary {
    modules: usize,
    source_sets: usize,
    kotlin_files: usize,
    java_files: usize,
    libraries: usize,
    library_failures: usize,
    model_time: Duration,
    read_time: Duration,
    library_time: Duration,
}

impl LoadSummary {
    fn of(loaded: &LoadedFiles) -> LoadSummary {
        let language_count = |extension: &str| {
            loaded
                .files
                .iter()
                .filter(|(path, _)| path.extension().is_some_and(|found| found == extension))
                .count()
        };
        LoadSummary {
            modules: loaded.model.modules.len(),
            source_sets: loaded.model.source_sets.len(),
            kotlin_files: language_count("kt") + language_count("kts"),
            java_files: language_count("java"),
            libraries: loaded.libraries.libraries.len(),
            library_failures: loaded.libraries.failures.len(),
            model_time: loaded.model_time,
            read_time: loaded.discovery_time + loaded.read_time,
            library_time: loaded.library_time,
        }
    }

    fn emit(&self, generation: u64, apply_time: Duration) {
        let resident_megabytes =
            memory_stats::memory_stats().map_or(0, |stats| stats.physical_mem / (1024 * 1024));
        Event::info("load-finished")
            .count("generation", generation)
            .count("modules", self.modules)
            .count("source-sets", self.source_sets)
            .count("kotlin-files", self.kotlin_files)
            .count("java-files", self.java_files)
            .count("libraries", self.libraries)
            .count("library-failures", self.library_failures)
            .duration("model-time", self.model_time)
            .duration("read-time", self.read_time)
            .duration("library-time", self.library_time)
            .duration("apply-time", apply_time)
            .count("memory-mb", resident_megabytes)
            .emit();
    }
}

/// Why a workspace load ended without a workspace.
enum LoadFailure {
    Model(project_model::LoadError),
    /// The loader panicked (the log has where).
    Panicked,
}

impl LoadFailure {
    /// For the log.
    fn kind(&self) -> &'static str {
        match self {
            LoadFailure::Model(project_model::LoadError::Unreadable(_)) => "model-unreadable",
            LoadFailure::Model(project_model::LoadError::Malformed(_)) => "model-malformed",
            LoadFailure::Panicked => "panicked",
        }
    }
}

/// For the user.
impl std::fmt::Display for LoadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadFailure::Model(error) => write!(formatter, "{error}"),
            LoadFailure::Panicked => {
                write!(
                    formatter,
                    "kotlin-analyzer failed while loading; its log says where"
                )
            }
        }
    }
}

fn loading_token(generation: u64) -> String {
    format!("{LOADING_TOKEN}/{generation}")
}

fn cast_notification<N>(notification: Notification) -> Result<N::Params, Notification>
where
    N: lsp_types::notification::Notification,
    N::Params: serde::de::DeserializeOwned,
{
    notification
        .extract(N::METHOD)
        .map_err(|error| match error {
            ExtractError::MethodMismatch(notification) => notification,
            ExtractError::JsonError { method, .. } => {
                Notification::new(method, serde_json::Value::Null)
            }
        })
}
