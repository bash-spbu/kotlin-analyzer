//! The language server: LSP over stdio.
//!
//! One main thread owns the editor state (open documents, the file system
//! view, the database host). Requests run on a thread pool, each against a
//! database snapshot; an edit cancels snapshots still computing, and their
//! requests are answered "content modified". Loading the workspace runs in
//! the background and reports progress.

mod agent;
mod code_lens;
mod convert;
mod editing;
mod formatting;
mod gradle_export;
mod handlers;
mod hierarchy;
mod library_sources;
mod progress;
mod pull_diagnostics;
mod semantic_tokens;
mod settings;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(crate) use convert::uri_of;
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
    // `workspace.diagnostics.refreshSupport` (LSP 3.17), which
    // `lsp_types`' capabilities don't have.
    let diagnostics_refresh =
        initialize_params["capabilities"]["workspace"]["diagnostics"]["refreshSupport"]
            .as_bool()
            .unwrap_or(false);
    let params: InitializeParams =
        serde_json::from_value(initialize_params).map_err(|error| error.to_string())?;
    let folders = workspace_folders(&params);
    let root = folders[0].clone();
    let project_json = params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("projectJson"))
        .and_then(|path| path.as_str())
        .map(PathBuf::from);
    let diagnostics_config = settings::diagnostics_config(params.initialization_options.as_ref());
    let formatting = settings::formatting_config(params.initialization_options.as_ref());
    let code_lens_config = settings::code_lens_config(params.initialization_options.as_ref());
    let agent_sockets = settings::agent_socket_directory(params.initialization_options.as_ref());
    let gradle_export = params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("gradleExport"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let log_level = settings::log_level(params.initialization_options.as_ref());
    crate::log::start_session(&root, log_level);
    log_session_start(
        &params,
        &root,
        &diagnostics_config,
        &formatting,
        gradle_export,
    );
    let encoding = position_encoding(&params.capabilities);
    let result = InitializeResult {
        capabilities: lsp_types::ServerCapabilities {
            position_encoding: Some(match encoding {
                vfs::ColumnUnit::Utf8 => lsp_types::PositionEncodingKind::UTF8,
                vfs::ColumnUnit::Utf16 => lsp_types::PositionEncodingKind::UTF16,
            }),
            ..capabilities()
        },
        server_info: Some(ServerInfo {
            name: "kotlin-analyzer".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    let mut result = serde_json::to_value(result).map_err(|error| error.to_string())?;
    // LSP 3.17's type hierarchy, which `lsp_types`' capabilities don't have.
    result["capabilities"]["typeHierarchyProvider"] = serde_json::Value::Bool(true);
    connection
        .initialize_finish(initialize_id, result)
        .map_err(|error| error.to_string())?;
    let mut state = GlobalState::new(connection.sender.clone(), root, project_json);
    state.folders = folders;
    state.encoding = encoding;
    state.pull_diagnostics = params
        .capabilities
        .text_document
        .as_ref()
        .is_some_and(|text_document| text_document.diagnostic.is_some());
    state.diagnostics_refresh = diagnostics_refresh;
    state.refreshes = ClientRefreshes::of(&params.capabilities);
    if watches_files(&params.capabilities) {
        state.register_file_watchers();
    }
    state.diagnostics_config = Arc::new(diagnostics_config);
    state.formatting = Arc::new(formatting);
    state.code_lens_config = code_lens_config;
    if let Some(directory) = agent_sockets {
        state.agent_sockets = agent::listen(&directory, &state.folders, &state.task_sender);
    }
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
    formatting: &formatting::FormattingConfig,
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
        .text("formatter", formatting.formatter_name())
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
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        definition_provider: Some(OneOf::Left(true)),
        declaration_provider: Some(lsp_types::DeclarationCapability::Simple(true)),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        diagnostic_provider: Some(lsp_types::DiagnosticServerCapabilities::Options(
            lsp_types::DiagnosticOptions {
                identifier: Some("kotlin-analyzer".to_owned()),
                inter_file_dependencies: true,
                workspace_diagnostics: true,
                work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
            },
        )),
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
        selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(true)),
        code_lens_provider: Some(lsp_types::CodeLensOptions {
            resolve_provider: Some(true),
        }),
        semantic_tokens_provider: Some(
            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                lsp_types::SemanticTokensOptions {
                    legend: editing::semantic_tokens_legend(),
                    full: Some(lsp_types::SemanticTokensFullOptions::Delta { delta: Some(true) }),
                    range: Some(true),
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
        document_formatting_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        workspace: Some(lsp_types::WorkspaceServerCapabilities {
            workspace_folders: Some(lsp_types::WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: Some(lsp_types::WorkspaceFileOperationsServerCapabilities {
                will_rename: Some(kotlin_files_and_folders()),
                did_rename: Some(kotlin_files_and_folders()),
                ..lsp_types::WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
        execute_command_provider: Some(lsp_types::ExecuteCommandOptions {
            commands: vec![RELOAD_COMMAND.to_owned()],
            ..lsp_types::ExecuteCommandOptions::default()
        }),
        ..ServerCapabilities::default()
    }
}

/// UTF-8 columns when the client offers them — the text's own unit,
/// nothing to count — else UTF-16, LSP's default.
fn position_encoding(capabilities: &lsp_types::ClientCapabilities) -> vfs::ColumnUnit {
    let offered = capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref());
    let utf8 =
        offered.is_some_and(|encodings| encodings.contains(&lsp_types::PositionEncodingKind::UTF8));
    if utf8 {
        vfs::ColumnUnit::Utf8
    } else {
        vfs::ColumnUnit::Utf16
    }
}

/// What file operations the server wants to hear of: Kotlin files, and
/// folders (which may hold some).
fn kotlin_files_and_folders() -> lsp_types::FileOperationRegistrationOptions {
    let filter = |glob: &str, matches| lsp_types::FileOperationFilter {
        scheme: Some("file".to_owned()),
        pattern: lsp_types::FileOperationPattern {
            glob: glob.to_owned(),
            matches: Some(matches),
            options: None,
        },
    };
    lsp_types::FileOperationRegistrationOptions {
        filters: vec![
            filter("**/*.{kt,kts}", lsp_types::FileOperationPatternKind::File),
            filter("**/*", lsp_types::FileOperationPatternKind::Folder),
        ],
    }
}

/// The workspace folders; without any, the (deprecated) root URI, else the
/// current directory — the first is the primary one (the log's, a given
/// model file's).
fn workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    let folders: Vec<PathBuf> = params
        .workspace_folders
        .iter()
        .flatten()
        .filter_map(|folder| convert::path_of(&folder.uri))
        .collect();
    if folders.is_empty() {
        vec![workspace_root(params)]
    } else {
        folders
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
    /// The background Gradle exports (one per build) finished, after `took`.
    ModelExported {
        exports: Vec<gradle_export::Export>,
        took: Duration,
    },
    /// A request on the background pool is done (answered, cancelled or
    /// failed).
    BackgroundFinished,
    /// Progress of work off the main thread, to send (see `progress`).
    Progress {
        token: String,
        /// The progress begins: ask the client to create its token first.
        create: bool,
        progress: WorkDoneProgress,
    },
    /// An agent's query, from a socket (see `agent`).
    Agent {
        request: crate::agent::protocol::AgentRequest,
        replier: agent::Replier,
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
    code_lenses: bool,
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
            code_lenses: workspace
                .and_then(|workspace| workspace.code_lens.as_ref())
                .and_then(|lenses| lenses.refresh_support)
                .unwrap_or(false),
        }
    }
}

struct GlobalState {
    sender: Sender<Message>,
    /// The primary workspace folder.
    root: PathBuf,
    /// Every workspace folder, the primary first: read as one workspace.
    folders: Vec<PathBuf>,
    /// Where the exported project model is, when not in the workspace.
    project_json: Option<PathBuf>,
    host: AnalysisHost,
    vfs: Vfs,
    model: Arc<ProjectModel>,
    /// Open documents and their versions.
    open_documents: FxHashMap<FileId, i32>,
    loaded: bool,
    pool: rayon::ThreadPool,
    /// Workspace-wide work (checking every file) runs here, on half the
    /// threads: what the user is waiting for — hover, completion — never
    /// queues behind it.
    background_pool: rayon::ThreadPool,
    /// Numbers the progress of each workspace check.
    next_check: u64,
    /// How many requests run on the background pool.
    background_running: usize,
    /// When to tell the user everything is ready.
    readiness: Readiness,
    task_sender: Sender<Task>,
    task_receiver: Receiver<Task>,
    /// Paths by file id, shared with request threads.
    paths: Arc<Vec<PathBuf>>,
    next_request: i32,
    shutdown_requested: bool,
    /// Which checks run, from the client's settings.
    diagnostics_config: Arc<ide::DiagnosticsConfig>,
    /// How documents are formatted, from the client's settings.
    formatting: Arc<formatting::FormattingConfig>,
    /// Which code lenses the client wants.
    code_lens_config: code_lens::CodeLensConfig,
    /// The sockets agents ask on, one per workspace folder no other host
    /// listens for.
    agent_sockets: Vec<agent::ListeningSocket>,
    /// Agents' queries that came before the workspace loaded.
    pending_agent_requests: Vec<(crate::agent::protocol::AgentRequest, agent::Replier)>,
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
    /// The requests sent to the pool and not answered yet, each with what
    /// tells its job it was cancelled.
    in_flight: FxHashMap<RequestId, Arc<AtomicBool>>,
    /// The semantic tokens last sent per open file (see `semantic_tokens`).
    sent_tokens: Arc<semantic_tokens::SentTokens>,
    /// What a position's column counts, as agreed with the client.
    encoding: vfs::ColumnUnit,
    /// How many times files, the model or the settings changed.
    revision: u64,
    /// The client asks for diagnostics (`textDocument/diagnostic`) rather
    /// than being sent them; and whether it can be asked to ask again.
    pull_diagnostics: bool,
    diagnostics_refresh: bool,
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
            folders: vec![root.clone()],
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
            background_pool: rayon::ThreadPoolBuilder::new()
                .num_threads((threads / 2).max(1))
                .thread_name(|index| format!("kotlin-analyzer-background-{index}"))
                .panic_handler(|_| {})
                .build()
                .expect("a thread pool of a few threads can be built"),
            next_check: 0,
            background_running: 0,
            readiness: Readiness::default(),
            task_sender,
            task_receiver,
            paths: Arc::default(),
            next_request: 0,
            shutdown_requested: false,
            diagnostics_config: Arc::default(),
            formatting: Arc::default(),
            code_lens_config: code_lens::CodeLensConfig::default(),
            agent_sockets: Vec::new(),
            pending_agent_requests: Vec::new(),
            gradle_export: true,
            exporting: false,
            load_generation: 0,
            diagnostics_generations: FxHashMap::default(),
            next_diagnostics_generation: 0,
            refreshes: ClientRefreshes::default(),
            requests_received: 0,
            in_flight: FxHashMap::default(),
            sent_tokens: Arc::default(),
            encoding: vfs::ColumnUnit::Utf16,
            revision: 0,
            pull_diagnostics: false,
            diagnostics_refresh: false,
        }
    }

    fn main_loop(&mut self, receiver: &Receiver<Message>) -> Result<Ending, String> {
        loop {
            let settled = self
                .readiness
                .settle
                .clone()
                .unwrap_or_else(crossbeam_channel::never);
            let shown = self
                .readiness
                .shown
                .as_ref()
                .map_or_else(crossbeam_channel::never, |(timer, _)| timer.clone());
            select! {
                recv(settled) -> _ => {
                    self.readiness.settle = None;
                    self.on_settled();
                }
                recv(shown) -> _ => {
                    if let Some((_, token)) = self.readiness.shown.take() {
                        self.progress_on(&token, WorkDoneProgress::End(WorkDoneProgressEnd { message: None }));
                    }
                }
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
                title: progress::title("Loading workspace"),
                cancellable: Some(false),
                message: Some(self.root.display().to_string()),
                percentage: None,
            }),
        );
        let project_json = self.model_file();
        let folders: Vec<(PathBuf, Option<PathBuf>)> = self
            .folders
            .iter()
            .map(|folder| (folder.clone(), self.model_file_of(folder)))
            .collect();
        Event::info("load-started")
            .count("generation", generation)
            .text("model", self.model_source(project_json.is_some()))
            .count("folders", folders.len())
            .emit();
        let sender = self.task_sender.clone();
        let progress = progress::Progress::begun(self.task_sender.clone(), token);
        std::thread::Builder::new()
            .name("kotlin-analyzer-loader".to_owned())
            .spawn(move || {
                // A panic (logged by the hook) still ends the load: the
                // editor isn't left waiting for it.
                let loaded = crate::log::within(Context::Loading { generation }, || {
                    std::panic::catch_unwind(|| {
                        #[cfg(test)]
                        if folders[0].0.join("panic-while-loading").exists() {
                            panic!("a test panic while loading");
                        }
                        load::read_workspaces_reporting(&folders, &|step| {
                            let (message, percentage) = match step {
                                load::LoadStep::ReadingModel => {
                                    ("Reading the project model".to_owned(), None)
                                }
                                load::LoadStep::ReadingFiles { count } => {
                                    (format!("Reading {count} files"), None)
                                }
                                load::LoadStep::IndexingLibraries { indexed, total } => (
                                    format!("Indexing libraries {indexed}/{total}"),
                                    progress::percentage(indexed, total),
                                ),
                            };
                            progress.report(message, percentage);
                        })
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
        self.model_file_of(&self.root)
    }

    /// `folder`'s model file: the one given (for the primary folder), the
    /// one exported to the cache (when there is one); `None` for a folder
    /// with its own `.kotlin-analyzer/project.json` (read from where it
    /// is) or none (its directories read by convention).
    fn model_file_of(&self, folder: &std::path::Path) -> Option<PathBuf> {
        let primary = folder == self.root;
        if primary && self.project_json.is_some() {
            return self.project_json.clone();
        }
        if folder.join(".kotlin-analyzer/project.json").is_file() {
            return None;
        }
        gradle_export::cached_model(folder).filter(|cached| cached.is_file())
    }

    /// Exports the Gradle model in the background when it would come from
    /// the cache and isn't current there (the build files are walked off
    /// the main thread); an export that changed it reloads.
    fn start_export(&mut self) {
        if !self.gradle_export || self.exporting {
            return;
        }
        // Each Gradle build among the folders whose model isn't given.
        let builds: Vec<(PathBuf, PathBuf)> = self
            .folders
            .iter()
            .filter(|folder| {
                let given = (**folder == self.root && self.project_json.is_some())
                    || folder.join(".kotlin-analyzer/project.json").is_file();
                !given && gradle_export::is_gradle_build(folder)
            })
            .filter_map(|folder| Some((folder.clone(), gradle_export::cached_model(folder)?)))
            .collect();
        if builds.is_empty() {
            return;
        }
        self.exporting = true;
        self.send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: lsp_types::NumberOrString::String(EXPORT_TOKEN.to_owned()),
        });
        self.progress_on(
            EXPORT_TOKEN,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: progress::title("Exporting the Gradle model"),
                cancellable: Some(false),
                message: Some("gradle kotlinAnalyzerModel".to_owned()),
                percentage: None,
            }),
        );
        let sender = self.task_sender.clone();
        let tool = if self.root.join("gradlew").is_file() {
            "gradle-wrapper"
        } else {
            "gradle-on-path"
        };
        Event::info("gradle-export-started")
            .text("tool", tool)
            .count("builds", builds.len())
            .emit();
        std::thread::Builder::new()
            .name("kotlin-analyzer-gradle".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let exports = builds
                    .iter()
                    .map(|(root, cached)| {
                        crate::log::within(Context::GradleExport, || {
                            std::panic::catch_unwind(|| {
                                gradle_export::export_if_stale(root, cached)
                            })
                        })
                        .unwrap_or_else(|_| {
                            gradle_export::Export::Failed(gradle_export::ExportFailure::panicked())
                        })
                    })
                    .collect();
                let took = started.elapsed();
                let _ = sender.send(Task::ModelExported { exports, took });
            })
            .expect("a thread can be spawned");
    }

    fn on_task(&mut self, task: Task) {
        match task {
            Task::Response(response) => {
                // A request the client cancelled was answered then.
                if self.in_flight.remove(&response.id).is_some() {
                    self.respond(response);
                }
            }
            Task::ModelExported { exports, took } => {
                self.on_exports(exports, took);
                self.work_stopped();
            }
            Task::BackgroundFinished => {
                self.background_running = self.background_running.saturating_sub(1);
                self.work_stopped();
            }
            Task::Progress {
                token,
                create,
                progress,
            } => {
                if create {
                    self.send_request::<request::WorkDoneProgressCreate>(
                        WorkDoneProgressCreateParams {
                            token: lsp_types::NumberOrString::String(token.clone()),
                        },
                    );
                }
                self.progress_on(&token, progress);
            }
            Task::Agent { request, replier } => {
                if self.loaded {
                    self.answer_agent(request, replier);
                } else {
                    self.pending_agent_requests.push((request, replier));
                }
            }
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

    /// Every build's export ended: one that exported a model reloads the
    /// workspace (once); each failure is shown.
    fn on_exports(&mut self, exports: Vec<gradle_export::Export>, took: Duration) {
        self.exporting = false;
        let any_exported = exports
            .iter()
            .any(|export| matches!(export, gradle_export::Export::Exported));
        for export in exports {
            self.on_export(export, took);
        }
        if any_exported {
            self.start_loading();
        }
    }

    fn on_export(&mut self, export: gradle_export::Export, took: Duration) {
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
            gradle_export::Export::UpToDate | gradle_export::Export::Exported => {}
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
        self.revision += 1;
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
        for (request, replier) in std::mem::take(&mut self.pending_agent_requests) {
            self.answer_agent(request, replier);
        }
        self.readiness.loaded_generation = self.load_generation;
        self.readiness.files = files;
        self.work_stopped();
    }

    /// Something the user may be waiting for has stopped: in a moment,
    /// unless more starts, the user is told everything is ready.
    fn work_stopped(&mut self) {
        let unannounced = self.readiness.announced < self.readiness.loaded_generation;
        if unannounced {
            self.readiness.settle = Some(crossbeam_channel::after(READY_SETTLE));
        }
    }

    /// Shows "All set" for a moment once the latest load has applied, the
    /// Gradle export is done and no workspace check runs — once per load.
    fn on_settled(&mut self) {
        let latest_load_applied = self.readiness.loaded_generation == self.load_generation;
        let idle = !self.exporting && self.background_running == 0;
        let unannounced = self.readiness.announced < self.readiness.loaded_generation;
        if !(latest_load_applied && idle && unannounced) {
            return;
        }
        self.readiness.announced = self.load_generation;
        let token = format!("kotlin-analyzer/ready/{}", self.load_generation);
        self.send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: lsp_types::NumberOrString::String(token.clone()),
        });
        self.progress_on(
            &token,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: progress::title("All set"),
                cancellable: Some(false),
                message: Some(format!(
                    "{} files, nothing left to wait for",
                    self.readiness.files
                )),
                percentage: None,
            }),
        );
        Event::info("ready")
            .count("generation", self.load_generation)
            .emit();
        self.readiness.shown = Some((crossbeam_channel::after(READY_SHOWN), token));
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

    /// Workspace folders added or removed: the workspace is read again from
    /// the folders there are now (the first left is the primary one).
    fn on_folders_changed(&mut self, event: lsp_types::WorkspaceFoldersChangeEvent) {
        let removed: Vec<PathBuf> = event
            .removed
            .iter()
            .filter_map(|folder| convert::path_of(&folder.uri))
            .collect();
        self.folders.retain(|folder| !removed.contains(folder));
        for added in event
            .added
            .iter()
            .filter_map(|folder| convert::path_of(&folder.uri))
        {
            if !self.folders.contains(&added) {
                self.folders.push(added);
            }
        }
        match self.folders.first() {
            Some(first) => self.root = first.clone(),
            None => self.folders.push(self.root.clone()),
        }
        Event::info("workspace-folders-changed")
            .count("folders", self.folders.len())
            .emit();
        self.start_loading();
        self.start_export();
    }

    /// Files (or folders) renamed in the editor: what was at the old paths
    /// is gone, what's at the new ones read — sources open in the editor
    /// excepted, which it tells of itself.
    fn on_renamed_files(&mut self, renames: Vec<lsp_types::FileRename>) {
        let path_of = |uri: &str| {
            uri.parse::<lsp_types::Uri>()
                .ok()
                .and_then(|uri| convert::path_of(&uri))
        };
        let mut moved = 0usize;
        for rename in renames {
            let (Some(old), Some(new)) = (path_of(&rename.old_uri), path_of(&rename.new_uri))
            else {
                continue;
            };
            let gone: Vec<PathBuf> = self
                .vfs
                .paths()
                .iter()
                .filter(|path| path.starts_with(&old))
                .cloned()
                .collect();
            for path in gone {
                let open = self
                    .vfs
                    .file_id(&path)
                    .is_some_and(|file| self.open_documents.contains_key(&file));
                if !open {
                    self.vfs.set_contents(&path, None);
                }
            }
            let arrived = walkdir::WalkDir::new(&new)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.into_path())
                .filter(|path| syntax::Language::of_path(path).is_some());
            for path in arrived {
                let text = std::fs::read_to_string(&path).ok().map(Arc::from);
                self.vfs.set_contents(&path, text);
                moved += 1;
            }
        }
        Event::debug("files-renamed").count("sources", moved).emit();
        self.apply_vfs_changes();
        self.update_diagnostics(self.open_documents.keys().copied().collect());
    }

    fn is_model_file(&self, path: &std::path::Path) -> bool {
        let a_folders_model = self
            .folders
            .iter()
            .any(|folder| path == folder.join(".kotlin-analyzer/project.json"));
        a_folders_model || self.project_json.as_deref() == Some(path)
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
        self.refresh_code_lenses();
    }

    fn refresh_code_lenses(&mut self) {
        if self.refreshes.code_lenses {
            self.send_request::<request::CodeLensRefresh>(());
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
        if request.method == request::WillRenameFiles::METHOD {
            // What moves is found here, where the files and the model are.
            let moves = self.file_moves(&request);
            self.answer_on_pool(request, move |snapshot, request| {
                handlers::will_rename_files(&snapshot, request.id, &moves)
            });
            return;
        }
        if request.method == request::Formatting::METHOD {
            self.answer_on_pool(request, formatting::handle);
            return;
        }
        if request.method == request::WorkspaceDiagnosticRequest::METHOD {
            self.next_check += 1;
            let progress = progress::Progress::deferred(
                self.task_sender.clone(),
                format!("kotlin-analyzer/checking/{}", self.next_check),
                "Checking files",
                CHECK_PROGRESS_DELAY,
            );
            self.answer_on(PoolKind::Background, request, move |snapshot, request| {
                pull_diagnostics::workspace_request(&snapshot, request, &progress)
            });
            return;
        }
        self.answer_on_pool(request, handlers::handle);
    }

    /// Answers `request` with `handle` on a worker, against a snapshot: a
    /// panic answered as an error, a cancelled request skipped, the answer
    /// logged.
    fn answer_on_pool(
        &mut self,
        request: Request,
        handle: impl FnOnce(handlers::Snapshot, Request) -> Response + Send + 'static,
    ) {
        self.answer_on(PoolKind::Interactive, request, handle);
    }

    fn answer_on(
        &mut self,
        pool: PoolKind,
        request: Request,
        handle: impl FnOnce(handlers::Snapshot, Request) -> Response + Send + 'static,
    ) {
        let request_file = self.request_file_of(&request);
        let method = known_method(&request.method);
        let id = request_number(&request.id);
        let file = request_file.map(|file| self.file_ref(file));
        log_request(&request, id, method, file);
        let snapshot = self.snapshot(request_file);
        let sender = self.task_sender.clone();
        let received = Instant::now();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.in_flight.insert(request.id.clone(), cancelled.clone());
        let finished = match pool {
            PoolKind::Interactive => None,
            PoolKind::Background => {
                self.background_running += 1;
                Some(BackgroundGuard(self.task_sender.clone()))
            }
        };
        let pool = match pool {
            PoolKind::Interactive => &self.pool,
            PoolKind::Background => &self.background_pool,
        };
        pool.spawn(move || {
            let _finished = finished;
            // Cancelled while it waited for a worker: nothing to compute.
            if cancelled.load(Ordering::SeqCst) {
                return;
            }
            let started = Instant::now();
            let request_id = request.id.clone();
            let context = Context::Request { id, method, file };
            let handled = crate::log::within(context, || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(snapshot, request)))
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

    fn snapshot(&self, request_file: Option<FileId>) -> handlers::Snapshot {
        handlers::Snapshot {
            analysis: self.host.analysis(),
            paths: self.paths.clone(),
            loaded: self.loaded,
            request_file,
            diagnostics_config: self.diagnostics_config.clone(),
            formatting: self.formatting.clone(),
            code_lens_config: self.code_lens_config,
            sent_tokens: self.sent_tokens.clone(),
            encoding: self.encoding,
            revision: self.revision,
        }
    }

    /// Answers an agent's query on a worker, against a snapshot; a query
    /// an edit cancelled says to ask again.
    fn answer_agent(
        &mut self,
        request: crate::agent::protocol::AgentRequest,
        replier: agent::Replier,
    ) {
        let path = request.query.file().map(std::path::Path::to_path_buf);
        let file = path.as_deref().and_then(|path| {
            self.vfs.file_id(path).or_else(|| {
                path.canonicalize()
                    .ok()
                    .and_then(|path| self.vfs.file_id(&path))
            })
        });
        let root = path
            .as_deref()
            .and_then(|path| self.folders.iter().find(|folder| path.starts_with(folder)))
            .unwrap_or(&self.root)
            .clone();
        let snapshot = self.snapshot(file);
        let kind = request.query.kind();
        self.pool.spawn(move || {
            let started = Instant::now();
            let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                agent::answer(&snapshot, &root, &request.query)
            }));
            let reply = match answered {
                Ok(Ok(answer)) => answer,
                Ok(Err(_cancelled)) => {
                    Err("the project changed while answering; ask again".to_owned())
                }
                Err(_) => {
                    Err("kotlin-analyzer failed on this query; its log says where".to_owned())
                }
            };
            Event::debug("agent-query")
                .text("query", kind)
                .flag("file-known", file.is_some())
                .text(
                    "outcome",
                    if reply.is_ok() { "answered" } else { "refused" },
                )
                .duration("took", started.elapsed())
                .emit();
            replier.send(&crate::agent::protocol::AgentReply::of(request.id, reply));
        });
    }

    /// `$/cancelRequest`: a request not answered yet is answered
    /// "cancelled" now; its work is skipped if it hasn't started, and its
    /// answer dropped when it comes (a computation already running goes
    /// on until done or an edit cancels it).
    fn cancel_request(&mut self, id: lsp_types::NumberOrString) {
        let id = match id {
            lsp_types::NumberOrString::Number(number) => RequestId::from(number),
            lsp_types::NumberOrString::String(text) => RequestId::from(text),
        };
        let Some(cancelled) = self.in_flight.remove(&id) else {
            return;
        };
        cancelled.store(true, Ordering::SeqCst);
        Event::debug("request-cancelled")
            .number("id", request_number(&id))
            .emit();
        self.respond(Response::new_err(
            id,
            lsp_server::ErrorCode::RequestCanceled as i32,
            "cancelled".to_owned(),
        ));
    }

    /// The Kotlin files a `workspace/willRenameFiles` renames or moves —
    /// a folder's, all under it — with the package their new directory
    /// implies.
    fn file_moves(&self, request: &Request) -> Vec<ide::FileMove> {
        let Ok(params) =
            serde_json::from_value::<lsp_types::RenameFilesParams>(request.params.clone())
        else {
            return Vec::new();
        };
        let path_of = |uri: &str| {
            uri.parse::<lsp_types::Uri>()
                .ok()
                .and_then(|uri| convert::path_of(&uri))
        };
        let mut moves = Vec::new();
        for rename in params.files {
            let (Some(old), Some(new)) = (path_of(&rename.old_uri), path_of(&rename.new_uri))
            else {
                continue;
            };
            for path in self.vfs.paths() {
                let Ok(rest) = path.strip_prefix(&old) else {
                    continue;
                };
                let is_kotlin = syntax::Language::of_path(path) == Some(syntax::Language::Kotlin);
                let Some(file) = self.vfs.file_id(path).filter(|_| is_kotlin) else {
                    continue;
                };
                let new_path = if rest.as_os_str().is_empty() {
                    new.clone()
                } else {
                    new.join(rest)
                };
                let stem = |path: &std::path::Path| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                moves.push(ide::FileMove {
                    file,
                    old_stem: stem(path),
                    new_stem: stem(&new_path),
                    new_directory_package: self.model.directory_package(&new_path),
                });
            }
        }
        moves
    }

    /// A file as the log names it: its path hashed relative to its folder.
    fn file_ref(&self, file: FileId) -> FileRef {
        let path = self.vfs.path(file);
        let folder = self
            .folders
            .iter()
            .find(|folder| path.starts_with(folder))
            .unwrap_or(&self.root);
        FileRef::new(file.index() as u32, path, folder)
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
        // `completionItem/resolve` carries it in the item's `data`, a
        // hierarchy's expansion in its `item.data`.
        let document = request
            .params
            .get("textDocument")
            .or_else(|| {
                request
                    .params
                    .get("data")
                    .and_then(|data| data.get("textDocument"))
            })
            .or_else(|| {
                request
                    .params
                    .get("item")
                    .and_then(|item| item.get("data"))
                    .and_then(|data| data.get("textDocument"))
            });
        let uri: lsp_types::Uri = document
            .and_then(|document| document.get("uri"))
            .and_then(|uri| serde_json::from_value(uri.clone()).ok())?;
        let path = convert::path_of(&uri)?;
        self.vfs.file_id(&path)
    }

    fn on_notification(&mut self, notification: Notification) {
        let notification = match cast_notification::<notification::Cancel>(notification) {
            Ok(params) => {
                self.cancel_request(params.id);
                return;
            }
            Err(notification) => notification,
        };
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
                    // Incremental sync: each change edits the text as the
                    // ones before it left it.
                    let changes = params.content_changes.len();
                    let current = self
                        .vfs
                        .file_id(&path)
                        .and_then(|file| self.vfs.contents(file).cloned())
                        .unwrap_or_default();
                    let text =
                        convert::apply_changes(&current, params.content_changes, self.encoding);
                    let shape = TextShape::of(&text);
                    let file = self.vfs.set_contents(&path, Some(Arc::from(text)));
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
            match cast_notification::<notification::DidChangeWorkspaceFolders>(notification) {
                Ok(params) => {
                    self.on_folders_changed(params.event);
                    return;
                }
                Err(notification) => notification,
            };
        let notification = match cast_notification::<notification::DidRenameFiles>(notification) {
            Ok(params) => {
                self.on_renamed_files(params.files);
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
                    if let Some(config) = settings::changed_code_lens_config(&params.settings) {
                        Event::info("code-lens-changed")
                            .flag("references", config.references)
                            .flag("tests", config.tests)
                            .emit();
                        self.code_lens_config = config;
                        self.refresh_code_lenses();
                    }
                    if let Some(config) = settings::changed_formatting_config(&params.settings) {
                        Event::info("formatting-changed")
                            .text("formatter", config.formatter_name())
                            .emit();
                        self.formatting = Arc::new(config);
                    }
                    if let Some(config) = settings::changed_diagnostics_config(&params.settings) {
                        let disabled: Vec<&'static str> =
                            config.disabled.iter().map(|check| check.name()).collect();
                        Event::info("settings-changed")
                            .flag("experimental-checks", config.experimental)
                            .texts("disabled-checks", disabled)
                            .emit();
                        self.diagnostics_config = Arc::new(config);
                        self.revision += 1;
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
                self.sent_tokens.forget(file);
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
            self.revision += 1;
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
        // A client that pulls is told to ask again instead.
        if self.pull_diagnostics {
            if self.diagnostics_refresh {
                self.send_request::<request::WorkspaceDiagnosticRefresh>(());
            }
            return;
        }
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
            let encoding = self.encoding;
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
                        handlers::diagnostics(&analysis, file, &config, encoding)
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

/// When the user is told everything is ready: after a load, once the work
/// it set off has stopped.
#[derive(Default)]
struct Readiness {
    /// The load generation last applied, and its files.
    loaded_generation: u64,
    files: usize,
    /// The load generation announced last.
    announced: u64,
    /// Fires once work has stopped for `READY_SETTLE`.
    settle: Option<Receiver<Instant>>,
    /// Ends the shown "All set", with its progress token.
    shown: Option<(Receiver<Instant>, String)>,
}

/// How long nothing must start before "All set": a client asks for the
/// workspace's diagnostics just after a load ends.
#[cfg(not(test))]
const READY_SETTLE: Duration = Duration::from_millis(300);
#[cfg(test)]
const READY_SETTLE: Duration = Duration::from_millis(20);

/// How long "All set" stays in the status bar.
#[cfg(not(test))]
const READY_SHOWN: Duration = Duration::from_secs(3);
#[cfg(test)]
const READY_SHOWN: Duration = Duration::from_millis(50);

/// Tells the main loop a background request is done, however it ends.
struct BackgroundGuard(Sender<Task>);

impl Drop for BackgroundGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Task::BackgroundFinished);
    }
}

/// Which of the server's thread pools a request runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoolKind {
    /// What the user waits for.
    Interactive,
    /// Workspace-wide work, on fewer threads.
    Background,
}

/// A workspace check shows its progress only once it has run this long.
const CHECK_PROGRESS_DELAY: Duration = Duration::from_millis(500);

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
    "textDocument/selectionRange",
    "textDocument/codeLens",
    "codeLens/resolve",
    "textDocument/semanticTokens/full",
    "textDocument/semanticTokens/full/delta",
    "textDocument/semanticTokens/range",
    "textDocument/inlayHint",
    "inlayHint/resolve",
    "textDocument/codeAction",
    "textDocument/diagnostic",
    "workspace/diagnostic",
    "workspace/executeCommand",
    "workspace/willRenameFiles",
    "textDocument/prepareTypeHierarchy",
    "typeHierarchy/supertypes",
    "typeHierarchy/subtypes",
    "textDocument/prepareCallHierarchy",
    "callHierarchy/incomingCalls",
    "callHierarchy/outgoingCalls",
    "textDocument/formatting",
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
