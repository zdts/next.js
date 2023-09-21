#![feature(future_join)]
#![feature(min_specialization)]
#![feature(arbitrary_self_types)]
#![feature(async_fn_in_trait)]

pub mod devserver_options;
mod turbo_tasks_viz;

use std::{
    collections::HashSet,
    env::current_dir,
    future::{join, Future},
    io::{stdout, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf, MAIN_SEPARATOR},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use devserver_options::DevServerOptions;
use dunce::canonicalize;
use indexmap::IndexMap;
use next_core::{
    app_structure::find_app_dir_if_enabled,
    create_app_source, create_page_source, create_web_entry_source,
    dev_manifest::DevManifestContentSource,
    mode::NextMode,
    next_client::{get_client_chunking_context, get_client_compile_time_info},
    next_config::{load_next_config, load_rewrites},
    next_image::NextImageContentSource,
    pages_structure::find_pages_structure,
    router_source::NextRouterContentSource,
    source_map::NextSourceMapTraceContentSource,
    tracing_presets::{
        TRACING_NEXT_TARGETS, TRACING_NEXT_TURBOPACK_TARGETS, TRACING_NEXT_TURBO_TASKS_TARGETS,
    },
};
use owo_colors::OwoColorize;
use tracing_subscriber::{prelude::*, EnvFilter, Registry};
use turbo_tasks::{
    util::FormatDuration, StatsType, TransientInstance, TurboTasks, TurboTasksBackendApi,
    UpdateInfo, Value, Vc,
};
use turbopack_binding::{
    turbo::{
        tasks_env::{CustomProcessEnv, ProcessEnv},
        tasks_fs::{DiskFileSystem, FileSystem},
        tasks_memory::MemoryBackend,
    },
    turbopack::{
        cli_utils::{
            exit::ExitGuard,
            issue::{ConsoleUi, LogOptions},
            raw_trace::RawTraceLayer,
            trace_writer::TraceWriter,
            tracing_presets::TRACING_OVERVIEW_TARGETS,
        },
        core::{
            environment::ServerAddr,
            issue::{IssueReporter, IssueSeverity},
            resolve::parse::Request,
            server_fs::ServerFileSystem,
            PROJECT_FILESYSTEM_NAME,
        },
        dev::DevChunkingContext,
        dev_server::{
            introspect::IntrospectionSource,
            source::{
                combined::CombinedContentSource, router::PrefixedRouterContentSource,
                static_assets::StaticAssetsContentSource, ContentSource,
            },
            DevServer, DevServerBuilder,
        },
        env::dotenv::load_env,
        node::execution_context::ExecutionContext,
        turbopack::evaluate_context::node_build_environment,
    },
};

#[derive(Clone)]
pub enum EntryRequest {
    Relative(String),
    Module(String, String),
}

pub struct NextDevServerBuilder {
    turbo_tasks: Arc<TurboTasks<MemoryBackend>>,
    project_dir: String,
    root_dir: String,
    entry_requests: Vec<EntryRequest>,
    eager_compile: bool,
    hostname: Option<IpAddr>,
    issue_reporter: Option<Box<dyn IssueReporterProvider>>,
    port: Option<u16>,
    browserslist_query: String,
    log_level: IssueSeverity,
    show_all: bool,
    log_detail: bool,
    allow_retry: bool,
}

impl NextDevServerBuilder {
    pub fn new(
        turbo_tasks: Arc<TurboTasks<MemoryBackend>>,
        project_dir: String,
        root_dir: String,
    ) -> NextDevServerBuilder {
        NextDevServerBuilder {
            turbo_tasks,
            project_dir,
            root_dir,
            entry_requests: vec![],
            eager_compile: false,
            hostname: None,
            issue_reporter: None,
            port: None,
            browserslist_query: "last 1 Chrome versions, last 1 Firefox versions, last 1 Safari \
                                 versions, last 1 Edge versions"
                .to_owned(),
            log_level: IssueSeverity::Warning,
            show_all: false,
            log_detail: false,
            allow_retry: false,
        }
    }

    pub fn entry_request(mut self, entry_asset_path: EntryRequest) -> NextDevServerBuilder {
        self.entry_requests.push(entry_asset_path);
        self
    }

    pub fn eager_compile(mut self, eager_compile: bool) -> NextDevServerBuilder {
        self.eager_compile = eager_compile;
        self
    }

    pub fn hostname(mut self, hostname: IpAddr) -> NextDevServerBuilder {
        self.hostname = Some(hostname);
        self
    }

    pub fn port(mut self, port: u16) -> NextDevServerBuilder {
        self.port = Some(port);
        self
    }

    pub fn browserslist_query(mut self, browserslist_query: String) -> NextDevServerBuilder {
        self.browserslist_query = browserslist_query;
        self
    }

    pub fn log_level(mut self, log_level: IssueSeverity) -> NextDevServerBuilder {
        self.log_level = log_level;
        self
    }

    pub fn show_all(mut self, show_all: bool) -> NextDevServerBuilder {
        self.show_all = show_all;
        self
    }

    pub fn allow_retry(mut self, allow_retry: bool) -> NextDevServerBuilder {
        self.allow_retry = allow_retry;
        self
    }

    pub fn log_detail(mut self, log_detail: bool) -> NextDevServerBuilder {
        self.log_detail = log_detail;
        self
    }

    pub fn issue_reporter(
        mut self,
        issue_reporter: Box<dyn IssueReporterProvider>,
    ) -> NextDevServerBuilder {
        self.issue_reporter = Some(issue_reporter);
        self
    }

    /// Attempts to find an open port to bind.
    fn find_port(&self, host: IpAddr, port: u16, max_attempts: u16) -> Result<DevServerBuilder> {
        // max_attempts of 1 means we loop 0 times.
        let max_attempts = max_attempts - 1;
        let mut attempts = 0;
        loop {
            let current_port = port + attempts;
            let addr = SocketAddr::new(host, current_port);
            let listen_result = DevServer::listen(addr);

            if let Err(e) = &listen_result {
                if self.allow_retry && attempts < max_attempts {
                    // Returned error from `listen` is not `std::io::Error` but `anyhow::Error`,
                    // so we need to access its source to check if it is
                    // `std::io::ErrorKind::AddrInUse`.
                    let should_retry = e
                        .source()
                        .and_then(|e| {
                            e.downcast_ref::<std::io::Error>()
                                .map(|e| e.kind() == std::io::ErrorKind::AddrInUse)
                        })
                        .unwrap_or(false);

                    if should_retry {
                        println!(
                            "{} - Port {} is in use, trying {} instead",
                            "warn ".yellow(),
                            current_port,
                            current_port + 1
                        );
                        attempts += 1;
                        continue;
                    }
                }
            }

            return listen_result;
        }
    }

    pub async fn build(self) -> Result<DevServer> {
        let port = self.port.context("port must be set")?;
        let host = self.hostname.context("hostname must be set")?;

        let server = self.find_port(host, port, 10)?;

        let turbo_tasks = self.turbo_tasks;
        let project_dir = self.project_dir;
        let root_dir = self.root_dir;
        let eager_compile = self.eager_compile;
        let show_all = self.show_all;
        let log_detail = self.log_detail;
        let browserslist_query = self.browserslist_query;
        let log_options = Arc::new(LogOptions {
            current_dir: current_dir().unwrap(),
            project_dir: PathBuf::from(project_dir.clone()),
            show_all,
            log_detail,
            log_level: self.log_level,
        });
        let entry_requests = Arc::new(self.entry_requests);
        let server_addr = Arc::new(server.addr);
        let tasks = turbo_tasks.clone();
        let issue_provider = self.issue_reporter.unwrap_or_else(|| {
            // Initialize a ConsoleUi reporter if no custom reporter was provided
            Box::new(move || Vc::upcast(ConsoleUi::new(log_options.clone().into())))
        });

        let source = move || {
            source(
                root_dir.clone(),
                project_dir.clone(),
                entry_requests.clone().into(),
                eager_compile,
                turbo_tasks.clone().into(),
                browserslist_query.clone(),
                server_addr.clone().into(),
            )
        };

        let issue_reporter_arc = Arc::new(move || issue_provider.get_issue_reporter());
        Ok(server.serve(tasks, source, issue_reporter_arc))
    }
}

#[turbo_tasks::function]
async fn project_fs(project_dir: String) -> Result<Vc<Box<dyn FileSystem>>> {
    let disk_fs = DiskFileSystem::new(PROJECT_FILESYSTEM_NAME.to_string(), project_dir.to_string());
    disk_fs.await?.start_watching_with_invalidation_reason()?;
    Ok(Vc::upcast(disk_fs))
}

#[turbo_tasks::function]
async fn output_fs(project_dir: String) -> Result<Vc<Box<dyn FileSystem>>> {
    let disk_fs = DiskFileSystem::new("output".to_string(), project_dir.to_string());
    disk_fs.await?.start_watching()?;
    Ok(Vc::upcast(disk_fs))
}

#[turbo_tasks::function]
async fn server_env(
    env: Vc<Box<dyn ProcessEnv>>,
    server_addr: Vc<ServerAddr>,
) -> Result<Vc<Box<dyn ProcessEnv>>> {
    let mut map = IndexMap::new();
    let addr = server_addr.await?;
    if let Some(port) = addr.port() {
        map.insert("PORT".to_string(), port.to_string());
    }
    if map.is_empty() {
        return Ok(env);
    }
    Ok(Vc::upcast(CustomProcessEnv::new(env, Vc::cell(map))))
}

#[turbo_tasks::function]
async fn source(
    root_dir: String,
    project_dir: String,
    entry_requests: TransientInstance<Vec<EntryRequest>>,
    eager_compile: bool,
    turbo_tasks: TransientInstance<TurboTasks<MemoryBackend>>,
    browserslist_query: String,
    server_addr: TransientInstance<SocketAddr>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let output_fs = output_fs(project_dir.clone());
    let fs = project_fs(root_dir.clone());
    let project_relative = project_dir.strip_prefix(&root_dir).unwrap_or_else(|| {
        panic!(
            "project directory '{project_dir}' exists outside of the root directory '{root_dir}'"
        )
    });
    let project_relative = project_relative
        .strip_prefix(MAIN_SEPARATOR)
        .unwrap_or(project_relative)
        .replace(MAIN_SEPARATOR, "/");
    let project_path = fs.root().join(project_relative);

    let server_addr = ServerAddr::new(*server_addr).cell();

    // [Note]: this is to conform changed signature to inject process.env.__NEXT_DIST_DIR,
    // while we don't use this devserver for now
    let dist_dir = ".next".to_string();

    let env = load_env(project_path);
    let env = server_env(env, server_addr);
    let build_output_root = output_fs
        .root()
        .join(dist_dir.clone())
        .join("build".to_string());
    let dist_root = output_fs.root().join(dist_dir.clone());

    let build_chunking_context = DevChunkingContext::builder(
        project_path,
        build_output_root,
        build_output_root.join("chunks".to_string()),
        build_output_root.join("assets".to_string()),
        node_build_environment(),
    )
    .build();

    let execution_context =
        ExecutionContext::new(project_path, Vc::upcast(build_chunking_context), env);

    let mode = NextMode::DevServer;
    let next_config_execution_context = execution_context.with_layer("next_config".to_string());
    let next_config = load_next_config(next_config_execution_context);
    let rewrites = load_rewrites(next_config_execution_context);

    let output_root = output_fs
        .root()
        .join(dist_dir.clone())
        .join("server".to_string());

    let dev_server_fs = Vc::upcast::<Box<dyn FileSystem>>(ServerFileSystem::new());
    let dev_server_root = dev_server_fs.root();
    let entry_requests = entry_requests
        .iter()
        .map(|r| match r {
            EntryRequest::Relative(p) => {
                Request::relative(Value::new(p.clone().into()), Default::default(), false)
            }
            EntryRequest::Module(m, p) => {
                Request::module(m.clone(), Value::new(p.clone().into()), Default::default())
            }
        })
        .collect();

    let web_source = create_web_entry_source(
        project_path,
        execution_context,
        entry_requests,
        dev_server_root,
        eager_compile,
        browserslist_query.clone(),
        next_config,
    );
    let client_compile_time_info =
        get_client_compile_time_info(mode, browserslist_query, dist_root);
    let client_chunking_context = get_client_chunking_context(
        project_path,
        dev_server_root,
        client_compile_time_info.environment(),
        mode,
    );
    let pages_structure =
        find_pages_structure(project_path, dev_server_root, next_config.page_extensions());
    let page_source = create_page_source(
        pages_structure,
        project_path,
        dist_root,
        execution_context,
        output_root.join("pages".to_string()),
        dev_server_root,
        env,
        client_chunking_context,
        client_compile_time_info,
        next_config,
        server_addr,
    );
    let app_dir = find_app_dir_if_enabled(project_path);
    let app_source = create_app_source(
        app_dir,
        dist_root,
        project_path,
        execution_context,
        output_root.join("app".to_string()),
        dev_server_root,
        env,
        client_chunking_context,
        client_compile_time_info,
        next_config,
        server_addr,
    );
    let viz = Vc::upcast(turbo_tasks_viz::TurboTasksSource::new(turbo_tasks.into()));
    let static_source = Vc::upcast(StaticAssetsContentSource::new(
        String::new(),
        project_path.join("public".to_string()),
    ));
    let manifest_source = Vc::upcast(
        DevManifestContentSource {
            page_roots: vec![page_source],
            rewrites,
        }
        .cell(),
    );
    let main_source = CombinedContentSource::new(vec![
        manifest_source,
        static_source,
        app_source,
        page_source,
        web_source,
    ]);
    let introspect = Vc::upcast(
        IntrospectionSource {
            roots: HashSet::from([Vc::upcast(main_source)]),
        }
        .cell(),
    );
    let main_source = Vc::upcast(main_source);
    let source_map_trace = Vc::upcast(NextSourceMapTraceContentSource::new(main_source));
    let img_source = Vc::upcast(NextImageContentSource::new(main_source));
    let router_source = Vc::upcast(NextRouterContentSource::new(
        main_source,
        execution_context,
        next_config,
        server_addr,
        app_dir,
        pages_structure,
        dist_root,
    ));
    let source = Vc::upcast(
        PrefixedRouterContentSource {
            prefix: Default::default(),
            routes: vec![
                ("__turbopack__".to_string(), introspect),
                ("__turbo_tasks__".to_string(), viz),
                (
                    "__nextjs_original-stack-frame".to_string(),
                    source_map_trace,
                ),
                // TODO: Load path from next.config.js
                ("_next/image".to_string(), img_source),
            ],
            fallback: router_source,
        }
        .cell(),
    );

    Ok(source)
}

pub fn register() {
    next_core::register();
    include!(concat!(env!("OUT_DIR"), "/register.rs"));
}

/// Start a devserver with the given options.
pub async fn start_server(options: &DevServerOptions) -> Result<()> {
    let start = Instant::now();

    #[cfg(feature = "tokio_console")]
    console_subscriber::init();

    let trace = std::env::var("NEXT_TURBOPACK_TRACING").ok();

    let _guard = if let Some(mut trace) = trace {
        // Trace presets
        match trace.as_str() {
            "overview" => {
                trace = TRACING_OVERVIEW_TARGETS.join(",");
            }
            "next" => {
                trace = TRACING_NEXT_TARGETS.join(",");
            }
            "turbopack" => {
                trace = TRACING_NEXT_TURBOPACK_TARGETS.join(",");
            }
            "turbo-tasks" => {
                trace = TRACING_NEXT_TURBO_TASKS_TARGETS.join(",");
            }
            _ => {}
        }

        let subscriber = Registry::default();

        let subscriber = subscriber.with(EnvFilter::builder().parse(trace).unwrap());

        let internal_dir = options
            .dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."))
            .join(".next");
        std::fs::create_dir_all(&internal_dir)
            .context("Unable to create .next directory")
            .unwrap();
        let trace_file = internal_dir.join("trace.log");
        let trace_writer = std::fs::File::create(trace_file).unwrap();
        let (trace_writer, guard) = TraceWriter::new(trace_writer);
        let subscriber = subscriber.with(RawTraceLayer::new(trace_writer));

        let guard = ExitGuard::new(guard).unwrap();

        subscriber.init();

        Some(guard)
    } else {
        None
    };

    register();

    let dir = options
        .dir
        .as_ref()
        .map(canonicalize)
        .unwrap_or_else(current_dir)
        .context("project directory can't be found")?
        .to_str()
        .context("project directory contains invalid characters")?
        .to_string();

    let root_dir = if let Some(root) = options.root.as_ref() {
        canonicalize(root)
            .context("root directory can't be found")?
            .to_str()
            .context("root directory contains invalid characters")?
            .to_string()
    } else {
        dir.clone()
    };

    let tt = TurboTasks::new(MemoryBackend::new(
        options.memory_limit.map_or(usize::MAX, |l| l * 1024 * 1024),
    ));

    let stats_type = match options.full_stats {
        true => StatsType::Full,
        false => StatsType::Essential,
    };
    tt.set_stats_type(stats_type);

    let tt_clone = tt.clone();

    #[allow(unused_mut)]
    let mut server = NextDevServerBuilder::new(tt, dir, root_dir)
        .entry_request(EntryRequest::Relative("src/index".into()))
        .eager_compile(options.eager_compile)
        .hostname(options.hostname)
        .port(options.port)
        .log_detail(options.log_detail)
        .show_all(options.show_all)
        .log_level(
            options
                .log_level
                .map_or_else(|| IssueSeverity::Warning, |l| l.0),
        );

    #[cfg(feature = "serializable")]
    {
        server = server.allow_retry(options.allow_retry);
    }

    let server = server.build().await?;

    {
        let index_uri = ServerAddr::new(server.addr).to_string()?;
        println!(
            "{} - started server on {}, url: {}",
            "ready".green(),
            server.addr,
            index_uri
        );
        if !options.no_open {
            let _ = webbrowser::open(&index_uri);
        }
    }

    let stats_future = async move {
        if options.log_detail {
            println!(
                "{event_type} - startup {start}",
                event_type = "event".purple(),
                start = FormatDuration(start.elapsed()),
            );
        }

        let mut progress_counter = 0;
        loop {
            let update_future = profile_timeout(
                tt_clone.as_ref(),
                tt_clone.aggregated_update_info(Duration::from_millis(100), Duration::MAX),
            );

            if let Some(UpdateInfo {
                duration: elapsed,
                tasks: count,
                reasons,
                ..
            }) = update_future.await
            {
                progress_counter = 0;
                match (options.log_detail, !reasons.is_empty()) {
                    (true, true) => {
                        println!(
                            "\x1b[2K{event_type} - {reasons} {elapsed} ({tasks} tasks)",
                            event_type = "event".purple(),
                            elapsed = FormatDuration(elapsed),
                            tasks = count,
                        );
                    }
                    (true, false) => {
                        println!(
                            "\x1b[2K{event_type} - compilation {elapsed} ({tasks} tasks)",
                            event_type = "event".purple(),
                            elapsed = FormatDuration(elapsed),
                            tasks = count,
                        );
                    }
                    (false, true) => {
                        println!(
                            "\x1b[2K{event_type} - {reasons} {elapsed}",
                            event_type = "event".purple(),
                            elapsed = FormatDuration(elapsed),
                        );
                    }
                    (false, false) => {
                        if elapsed > Duration::from_secs(1) {
                            println!(
                                "\x1b[2K{event_type} - compilation {elapsed}",
                                event_type = "event".purple(),
                                elapsed = FormatDuration(elapsed),
                            );
                        }
                    }
                }
            } else {
                progress_counter += 1;
                print!(
                    "\x1b[2K{event_type} - {progress_counter}s...\r",
                    event_type = "event".purple(),
                );
                let _ = stdout().lock().flush();
            }
        }
    };

    join!(stats_future, async { server.future.await.unwrap() }).await;

    Ok(())
}

#[cfg(feature = "profile")]
// When profiling, exits the process when no new updates have been received for
// a given timeout and there are no more tasks in progress.
async fn profile_timeout<T>(tt: &TurboTasks<MemoryBackend>, future: impl Future<Output = T>) -> T {
    /// How long to wait in between updates before force-exiting the process
    /// during profiling.
    const PROFILE_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

    futures::pin_mut!(future);
    loop {
        match tokio::time::timeout(PROFILE_EXIT_TIMEOUT, &mut future).await {
            Ok(res) => return res,
            Err(_) => {
                if tt.get_in_progress_count() == 0 {
                    std::process::exit(0)
                }
            }
        }
    }
}

#[cfg(not(feature = "profile"))]
fn profile_timeout<T>(
    _tt: &TurboTasks<MemoryBackend>,
    future: impl Future<Output = T>,
) -> impl Future<Output = T> {
    future
}

pub trait IssueReporterProvider: Send + Sync + 'static {
    fn get_issue_reporter(&self) -> Vc<Box<dyn IssueReporter>>;
}

impl<T> IssueReporterProvider for T
where
    T: Fn() -> Vc<Box<dyn IssueReporter>> + Send + Sync + Clone + 'static,
{
    fn get_issue_reporter(&self) -> Vc<Box<dyn IssueReporter>> {
        self()
    }
}
