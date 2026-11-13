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

/// The progress token of workspace loading (one per load: a reload may
/// start while one is running).
const LOADING_TOKEN: &str = "kotlin-analyzer/loading";
const EXPORT_TOKEN: &str = "kotlin-analyzer/gradle";

/// `workspace/executeCommand`: reload the project model and re-read the
/// workspace's files.
pub const RELOAD_COMMAND: &str = "kotlin-analyzer.reloadWorkspace";

pub fn run() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();
    run_on(&connection)?;
    drop(connection);
    io_threads.join().map_err(|error| error.to_string())
}

/// Serves `connection` until the client says `exit` (or goes away).
pub(crate) fn run_on(connection: &Connection) -> Result<(), String> {
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
    state.diagnostics_config = Arc::new(diagnostics_config);
    state.gradle_export = gradle_export;
    state.start_loading();
    state.start_export();
    state.main_loop(&connection.receiver)
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        completion_provider: Some(lsp_types::CompletionOptions {
            trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
            ..lsp_types::CompletionOptions::default()
        }),
        signature_help_provider: Some(lsp_types::SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
            retrigger_characters: Some(vec![",".to_owned()]),
            ..lsp_types::SignatureHelpOptions::default()
        }),
        references_provider: Some(OneOf::Left(true)),
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
        inlay_hint_provider: Some(OneOf::Left(true)),
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
        loaded: Result<LoadedFiles, String>,
    },
    /// The background Gradle export finished.
    ModelExported(gradle_export::Export),
    Diagnostics {
        file: FileId,
        version: i32,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
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
        }
    }

    fn main_loop(&mut self, receiver: &Receiver<Message>) -> Result<(), String> {
        loop {
            select! {
                recv(receiver) -> message => {
                    let Ok(message) = message else { return Ok(()) };
                    match message {
                        Message::Request(request) => {
                            if request.method == request::Shutdown::METHOD {
                                self.shutdown_requested = true;
                                self.respond(Response::new_ok(request.id, ()));
                                continue;
                            }
                            self.on_request(request);
                        }
                        Message::Notification(notification) => {
                            if notification.method == notification::Exit::METHOD {
                                return Ok(());
                            }
                            self.on_notification(notification);
                        }
                        Message::Response(_) => {}
                    }
                }
                recv(self.task_receiver) -> task => {
                    let Ok(task) = task else { return Ok(()) };
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
        let sender = self.task_sender.clone();
        std::thread::Builder::new()
            .name("kotlin-analyzer-loader".to_owned())
            .spawn(move || {
                let loaded = load::read_workspace(&root, project_json.as_deref());
                let _ = sender.send(Task::Loaded { generation, loaded });
            })
            .expect("a thread can be spawned");
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
        std::thread::Builder::new()
            .name("kotlin-analyzer-gradle".to_owned())
            .spawn(move || {
                let export = gradle_export::export_if_stale(&root, &cached);
                let _ = sender.send(Task::ModelExported(export));
            })
            .expect("a thread can be spawned");
    }

    fn on_task(&mut self, task: Task) {
        match task {
            Task::Response(response) => self.respond(response),
            Task::ModelExported(export) => self.on_export(export),
            Task::Loaded { generation, loaded } => {
                if generation != self.load_generation {
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
                diagnostics,
            } => {
                let current = self.open_documents.get(&file).copied();
                if current != Some(version) {
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

    fn on_export(&mut self, export: gradle_export::Export) {
        self.exporting = false;
        let message = match &export {
            gradle_export::Export::UpToDate => "up to date",
            gradle_export::Export::Exported => "exported",
            gradle_export::Export::Failed(_) => "failed",
        };
        self.progress_on(
            EXPORT_TOKEN,
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(message.to_owned()),
            }),
        );
        match export {
            gradle_export::Export::UpToDate => {}
            gradle_export::Export::Exported => self.start_loading(),
            gradle_export::Export::Failed(error) => {
                self.notify::<notification::ShowMessage>(lsp_types::ShowMessageParams {
                    typ: lsp_types::MessageType::WARNING,
                    message: format!(
                        "kotlin-analyzer couldn't export the Gradle model (libraries stay unknown):\n{error}"
                    ),
                })
            }
        }
    }

    fn on_loaded(&mut self, loaded: Result<LoadedFiles, String>) {
        let token = loading_token(self.load_generation);
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
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
        let files = loaded.files.len();
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
        self.progress_on(
            &token,
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(format!("{files} files")),
            }),
        );
        self.update_diagnostics(self.open_documents.keys().copied().collect());
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
        if request.method == request::ExecuteCommand::METHOD {
            self.execute_command(request);
            return;
        }
        let snapshot = handlers::Snapshot {
            analysis: self.host.analysis(),
            paths: self.paths.clone(),
            loaded: self.loaded,
            request_file: self.request_file_of(&request),
            diagnostics_config: self.diagnostics_config.clone(),
        };
        let sender = self.task_sender.clone();
        self.pool.spawn(move || {
            let response = handlers::handle(snapshot, request);
            let _ = sender.send(Task::Response(response));
        });
    }

    /// `workspace/executeCommand`, on the main thread: the commands change
    /// the server's state.
    fn execute_command(&mut self, request: Request) {
        let params: Result<lsp_types::ExecuteCommandParams, _> =
            serde_json::from_value(request.params.clone());
        match params {
            Ok(params) if params.command == RELOAD_COMMAND => {
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
        let uri: lsp_types::Uri = request
            .params
            .get("textDocument")
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
                        let file = self
                            .vfs
                            .set_contents(&path, Some(Arc::from(params.text_document.text)));
                        self.open_documents
                            .insert(file, params.text_document.version);
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
                    let Some(change) = params.content_changes.into_iter().last() else {
                        return;
                    };
                    let file = self.vfs.set_contents(&path, Some(Arc::from(change.text)));
                    self.open_documents
                        .insert(file, params.text_document.version);
                    self.apply_vfs_changes();
                    self.update_diagnostics(vec![file]);
                    return;
                }
                Err(notification) => notification,
            };
        let notification =
            match cast_notification::<notification::DidChangeConfiguration>(notification) {
                Ok(params) => {
                    if let Some(config) = settings::changed_diagnostics_config(&params.settings) {
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
                self.open_documents.remove(&file);
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

    fn update_diagnostics(&mut self, files: Vec<FileId>) {
        for file in files {
            let Some(version) = self.open_documents.get(&file).copied() else {
                continue;
            };
            let analysis = self.host.analysis();
            let sender = self.task_sender.clone();
            // Until the workspace is loaded only syntax is known for sure.
            let config = if self.loaded {
                self.diagnostics_config.clone()
            } else {
                Arc::new(ide::DiagnosticsConfig {
                    only: vec![ide::Check::Syntax],
                    ..ide::DiagnosticsConfig::default()
                })
            };
            self.pool.spawn(move || {
                let Ok(diagnostics) = handlers::diagnostics(&analysis, file, &config) else {
                    return;
                };
                let _ = sender.send(Task::Diagnostics {
                    file,
                    version,
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
