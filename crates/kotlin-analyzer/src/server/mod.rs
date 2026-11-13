//! The language server: LSP over stdio.
//!
//! One main thread owns the editor state (open documents, the file system
//! view, the database host). Requests run on a thread pool, each against a
//! database snapshot; an edit cancels snapshots still computing, and their
//! requests are answered "content modified". Loading the workspace runs in
//! the background and reports progress.

mod convert;
mod handlers;
mod library_sources;
mod settings;

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

/// The progress token of workspace loading.
const LOADING_TOKEN: &str = "kotlin-analyzer/loading";

pub fn run() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();
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
    state.start_loading();
    state.main_loop(&connection.receiver)?;
    drop(state);
    drop(connection);
    io_threads.join().map_err(|error| error.to_string())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
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
    Loaded(Result<LoadedFiles, String>),
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

    fn start_loading(&mut self) {
        self.send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: lsp_types::NumberOrString::String(LOADING_TOKEN.to_owned()),
        });
        self.progress(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "Loading workspace".to_owned(),
            cancellable: Some(false),
            message: Some(self.root.display().to_string()),
            percentage: None,
        }));
        let root = self.root.clone();
        let project_json = self.project_json.clone();
        let sender = self.task_sender.clone();
        std::thread::Builder::new()
            .name("kotlin-analyzer-loader".to_owned())
            .spawn(move || {
                let loaded = load::read_workspace(&root, project_json.as_deref());
                let _ = sender.send(Task::Loaded(loaded));
            })
            .expect("a thread can be spawned");
    }

    fn on_task(&mut self, task: Task) {
        match task {
            Task::Response(response) => self.respond(response),
            Task::Loaded(Ok(loaded)) => {
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
                // Open documents get their source sets now that the model exists.
                for file in self.open_documents.keys().copied().collect::<Vec<_>>() {
                    let path = self.vfs.path(file).to_path_buf();
                    let text = self.vfs.contents(file).cloned();
                    if let (Some(text), Some(language)) = (text, syntax::Language::of_path(&path)) {
                        self.host.apply_change(vec![hir::FileChange {
                            file,
                            text: Some(text),
                            language,
                            source_set: self.model.source_set_of(&path),
                            directory_package: self.model.directory_package(&path),
                        }]);
                    }
                }
                self.loaded = true;
                self.refresh_paths();
                self.progress(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(format!("{files} files")),
                }));
                self.update_diagnostics(self.open_documents.keys().copied().collect());
            }
            Task::Loaded(Err(error)) => {
                self.loaded = true;
                self.progress(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(format!("loading failed: {error}")),
                }));
                self.notify::<notification::ShowMessage>(lsp_types::ShowMessageParams {
                    typ: lsp_types::MessageType::ERROR,
                    message: format!("kotlin-analyzer: {error}"),
                });
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

    fn on_request(&mut self, request: Request) {
        if self.shutdown_requested {
            self.respond(Response::new_err(
                request.id,
                lsp_server::ErrorCode::InvalidRequest as i32,
                "shutdown was requested".to_owned(),
            ));
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

    fn progress(&self, progress: WorkDoneProgress) {
        self.notify::<notification::Progress>(lsp_types::ProgressParams {
            token: lsp_types::NumberOrString::String(LOADING_TOKEN.to_owned()),
            value: lsp_types::ProgressParamsValue::WorkDone(progress),
        });
    }
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
