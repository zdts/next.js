use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use napi::{
    bindgen_prelude::External,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    JsFunction, Status,
};
use next_api::{
    project::{Middleware, ProjectContainer, ProjectOptions},
    route::{Endpoint, Route},
};
use next_core::tracing_presets::{
    TRACING_NEXT_TARGETS, TRACING_NEXT_TURBOPACK_TARGETS, TRACING_NEXT_TURBO_TASKS_TARGETS,
};
use tracing_subscriber::{
    prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry,
};
use turbo_tasks::{TransientInstance, TurboTasks, UpdateInfo, Vc};
use turbopack_binding::{
    turbo::tasks_memory::MemoryBackend,
    turbopack::{
        cli_utils::{
            exit::ExitGuard,
            raw_trace::RawTraceLayer,
            trace_writer::{TraceWriter, TraceWriterGuard},
            tracing_presets::TRACING_OVERVIEW_TARGETS,
        },
        core::{
            error::PrettyPrintError,
            version::{PartialUpdate, TotalUpdate, Update},
        },
        ecmascript_hmr_protocol::{ClientUpdateInstruction, ResourceIdentifier},
    },
};

use super::{
    endpoint::ExternalEndpoint,
    utils::{
        get_diagnostics, get_issues, subscribe, NapiDiagnostic, NapiIssue, RootTask,
        TurbopackResult, VcArc,
    },
};
use crate::register;

#[napi(object)]
pub struct NapiEnvVar {
    pub name: String,
    pub value: String,
}

#[napi(object)]
pub struct NapiProjectOptions {
    /// A root path from which all files must be nested under. Trying to access
    /// a file outside this root will fail. Think of this as a chroot.
    pub root_path: String,

    /// A path inside the root_path which contains the app/pages directories.
    pub project_path: String,

    /// next.config's distDir. Project initialization occurs eariler than
    /// deserializing next.config, so passing it as separate option.
    pub dist_dir: Option<String>,

    /// Whether to watch he filesystem for file changes.
    pub watch: bool,

    /// The contents of next.config.js, serialized to JSON.
    pub next_config: String,

    /// The contents of ts/config read by load-jsconfig, serialized to JSON.
    pub js_config: String,

    /// A map of environment variables to use when compiling code.
    pub env: Vec<NapiEnvVar>,

    /// The address of the dev server.
    pub server_addr: String,
}

#[napi(object)]
pub struct NapiTurboEngineOptions {
    /// An upper bound of memory that turbopack will attempt to stay under.
    pub memory_limit: Option<f64>,
}

impl From<NapiProjectOptions> for ProjectOptions {
    fn from(val: NapiProjectOptions) -> Self {
        ProjectOptions {
            root_path: val.root_path,
            project_path: val.project_path,
            watch: val.watch,
            next_config: val.next_config,
            js_config: val.js_config,
            env: val
                .env
                .into_iter()
                .map(|NapiEnvVar { name, value }| (name, value))
                .collect(),
            server_addr: val.server_addr,
        }
    }
}

pub struct ProjectInstance {
    turbo_tasks: Arc<TurboTasks<MemoryBackend>>,
    container: Vc<ProjectContainer>,
    #[allow(dead_code)]
    guard: Option<ExitGuard<TraceWriterGuard>>,
}

#[napi(ts_return_type = "{ __napiType: \"Project\" }")]
pub async fn project_new(
    options: NapiProjectOptions,
    turbo_engine_options: NapiTurboEngineOptions,
) -> napi::Result<External<ProjectInstance>> {
    register();

    let trace = std::env::var("NEXT_TURBOPACK_TRACING").ok();

    let guard = if let Some(mut trace) = trace {
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
        let dist_dir = options
            .dist_dir
            .as_ref()
            .map_or_else(|| ".next".to_string(), |d| d.to_string());

        let internal_dir = PathBuf::from(&options.project_path).join(dist_dir);
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

    let turbo_tasks = TurboTasks::new(MemoryBackend::new(
        turbo_engine_options
            .memory_limit
            .map(|m| m as usize)
            .unwrap_or(usize::MAX),
    ));
    let options = options.into();
    let container = turbo_tasks
        .run_once(async move {
            let project = ProjectContainer::new(options);
            let project = project.resolve().await?;
            Ok(project)
        })
        .await
        .map_err(|e| napi::Error::from_reason(PrettyPrintError(&e).to_string()))?;
    Ok(External::new_with_size_hint(
        ProjectInstance {
            turbo_tasks,
            container,
            guard,
        },
        100,
    ))
}

#[napi(ts_return_type = "{ __napiType: \"Project\" }")]
pub async fn project_update(
    #[napi(ts_arg_type = "{ __napiType: \"Project\" }")] project: External<ProjectInstance>,
    options: NapiProjectOptions,
) -> napi::Result<()> {
    let turbo_tasks = project.turbo_tasks.clone();
    let options = options.into();
    let container = project.container;
    turbo_tasks
        .run_once(async move {
            container.update(options).await?;
            Ok(())
        })
        .await
        .map_err(|e| napi::Error::from_reason(PrettyPrintError(&e).to_string()))?;
    Ok(())
}

#[napi(object)]
#[derive(Default)]
struct NapiRoute {
    /// The relative path from project_path to the route file
    pub pathname: String,

    /// The type of route, eg a Page or App
    pub r#type: &'static str,

    // Different representations of the endpoint
    pub endpoint: Option<External<ExternalEndpoint>>,
    pub html_endpoint: Option<External<ExternalEndpoint>>,
    pub rsc_endpoint: Option<External<ExternalEndpoint>>,
    pub data_endpoint: Option<External<ExternalEndpoint>>,
}

impl NapiRoute {
    fn from_route(
        pathname: String,
        value: Route,
        turbo_tasks: &Arc<TurboTasks<MemoryBackend>>,
    ) -> Self {
        let convert_endpoint = |endpoint: Vc<Box<dyn Endpoint>>| {
            Some(External::new(ExternalEndpoint(VcArc::new(
                turbo_tasks.clone(),
                endpoint,
            ))))
        };
        match value {
            Route::Page {
                html_endpoint,
                data_endpoint,
            } => NapiRoute {
                pathname,
                r#type: "page",
                html_endpoint: convert_endpoint(html_endpoint),
                data_endpoint: convert_endpoint(data_endpoint),
                ..Default::default()
            },
            Route::PageApi { endpoint } => NapiRoute {
                pathname,
                r#type: "page-api",
                endpoint: convert_endpoint(endpoint),
                ..Default::default()
            },
            Route::AppPage {
                html_endpoint,
                rsc_endpoint,
            } => NapiRoute {
                pathname,
                r#type: "app-page",
                html_endpoint: convert_endpoint(html_endpoint),
                rsc_endpoint: convert_endpoint(rsc_endpoint),
                ..Default::default()
            },
            Route::AppRoute { endpoint } => NapiRoute {
                pathname,
                r#type: "app-route",
                endpoint: convert_endpoint(endpoint),
                ..Default::default()
            },
            Route::Conflict => NapiRoute {
                pathname,
                r#type: "conflict",
                ..Default::default()
            },
        }
    }
}

#[napi(object)]
struct NapiMiddleware {
    pub endpoint: External<ExternalEndpoint>,
}

impl NapiMiddleware {
    fn from_middleware(
        value: &Middleware,
        turbo_tasks: &Arc<TurboTasks<MemoryBackend>>,
    ) -> Result<Self> {
        Ok(NapiMiddleware {
            endpoint: External::new(ExternalEndpoint(VcArc::new(
                turbo_tasks.clone(),
                value.endpoint,
            ))),
        })
    }
}
#[napi(object)]
struct NapiEntrypoints {
    pub routes: Vec<NapiRoute>,
    pub middleware: Option<NapiMiddleware>,
    pub pages_document_endpoint: External<ExternalEndpoint>,
    pub pages_app_endpoint: External<ExternalEndpoint>,
    pub pages_error_endpoint: External<ExternalEndpoint>,
}

#[napi(ts_return_type = "{ __napiType: \"RootTask\" }")]
pub fn project_entrypoints_subscribe(
    #[napi(ts_arg_type = "{ __napiType: \"Project\" }")] project: External<ProjectInstance>,
    func: JsFunction,
) -> napi::Result<External<RootTask>> {
    let turbo_tasks = project.turbo_tasks.clone();
    let container = project.container;
    subscribe(
        turbo_tasks.clone(),
        func,
        move || async move {
            let entrypoints = container.entrypoints();
            let issues = get_issues(entrypoints).await?;
            let diags = get_diagnostics(entrypoints).await?;

            let entrypoints = entrypoints.strongly_consistent().await?;

            Ok((entrypoints, issues, diags))
        },
        move |ctx| {
            let (entrypoints, issues, diags) = ctx.value;

            Ok(vec![TurbopackResult {
                result: NapiEntrypoints {
                    routes: entrypoints
                        .routes
                        .iter()
                        .map(|(pathname, &route)| {
                            NapiRoute::from_route(pathname.clone(), route, &turbo_tasks)
                        })
                        .collect::<Vec<_>>(),
                    middleware: entrypoints
                        .middleware
                        .as_ref()
                        .map(|m| NapiMiddleware::from_middleware(m, &turbo_tasks))
                        .transpose()?,
                    pages_document_endpoint: External::new(ExternalEndpoint(VcArc::new(
                        turbo_tasks.clone(),
                        entrypoints.pages_document_endpoint,
                    ))),
                    pages_app_endpoint: External::new(ExternalEndpoint(VcArc::new(
                        turbo_tasks.clone(),
                        entrypoints.pages_app_endpoint,
                    ))),
                    pages_error_endpoint: External::new(ExternalEndpoint(VcArc::new(
                        turbo_tasks.clone(),
                        entrypoints.pages_error_endpoint,
                    ))),
                },
                issues: issues
                    .iter()
                    .map(|issue| NapiIssue::from(&**issue))
                    .collect(),
                diagnostics: diags.iter().map(|d| NapiDiagnostic::from(d)).collect(),
            }])
        },
    )
}

#[napi(ts_return_type = "{ __napiType: \"RootTask\" }")]
pub fn project_hmr_events(
    #[napi(ts_arg_type = "{ __napiType: \"Project\" }")] project: External<ProjectInstance>,
    identifier: String,
    func: JsFunction,
) -> napi::Result<External<RootTask>> {
    let turbo_tasks = project.turbo_tasks.clone();
    let project = project.container;
    let session = TransientInstance::new(());
    subscribe(
        turbo_tasks.clone(),
        func,
        {
            let identifier = identifier.clone();
            let session = session.clone();
            move || {
                let identifier = identifier.clone();
                let session = session.clone();
                async move {
                    let state = project
                        .project()
                        .hmr_version_state(identifier.clone(), session);
                    let update = project.project().hmr_update(identifier, state);
                    let issues = get_issues(update).await?;
                    let diags = get_diagnostics(update).await?;
                    let update = update.strongly_consistent().await?;
                    match &*update {
                        Update::None => {}
                        Update::Total(TotalUpdate { to }) => {
                            state.set(to.clone()).await?;
                        }
                        Update::Partial(PartialUpdate { to, .. }) => {
                            state.set(to.clone()).await?;
                        }
                    }
                    Ok((update, issues, diags))
                }
            }
        },
        move |ctx| {
            let (update, issues, diags) = ctx.value;

            let napi_issues = issues
                .iter()
                .map(|issue| NapiIssue::from(&**issue))
                .collect();
            let update_issues = issues
                .iter()
                .map(|issue| (&**issue).into())
                .collect::<Vec<_>>();

            let identifier = ResourceIdentifier {
                path: identifier.clone(),
                headers: None,
            };
            let update = match &*update {
                Update::Total(_) => ClientUpdateInstruction::restart(&identifier, &update_issues),
                Update::Partial(update) => ClientUpdateInstruction::partial(
                    &identifier,
                    &update.instruction,
                    &update_issues,
                ),
                Update::None => ClientUpdateInstruction::issues(&identifier, &update_issues),
            };

            Ok(vec![TurbopackResult {
                result: ctx.env.to_js_value(&update)?,
                issues: napi_issues,
                diagnostics: diags.iter().map(|d| NapiDiagnostic::from(d)).collect(),
            }])
        },
    )
}

#[napi(object)]
struct HmrIdentifiers {
    pub identifiers: Vec<String>,
}

#[napi(ts_return_type = "{ __napiType: \"RootTask\" }")]
pub fn project_hmr_identifiers_subscribe(
    #[napi(ts_arg_type = "{ __napiType: \"Project\" }")] project: External<ProjectInstance>,
    func: JsFunction,
) -> napi::Result<External<RootTask>> {
    let turbo_tasks = project.turbo_tasks.clone();
    let container = project.container;
    subscribe(
        turbo_tasks.clone(),
        func,
        move || async move {
            let hmr_identifiers = container.hmr_identifiers();
            let issues = get_issues(hmr_identifiers).await?;
            let diags = get_diagnostics(hmr_identifiers).await?;

            let hmr_identifiers = hmr_identifiers.strongly_consistent().await?;

            Ok((hmr_identifiers, issues, diags))
        },
        move |ctx| {
            let (hmr_identifiers, issues, diags) = ctx.value;

            Ok(vec![TurbopackResult {
                result: HmrIdentifiers {
                    identifiers: hmr_identifiers
                        .iter()
                        .map(|ident| ident.to_string())
                        .collect::<Vec<_>>(),
                },
                issues: issues
                    .iter()
                    .map(|issue| NapiIssue::from(&**issue))
                    .collect(),
                diagnostics: diags.iter().map(|d| NapiDiagnostic::from(d)).collect(),
            }])
        },
    )
}

#[napi(object)]
struct NapiUpdateInfo {
    pub duration: u32,
    pub tasks: u32,
}

impl From<UpdateInfo> for NapiUpdateInfo {
    fn from(update_info: UpdateInfo) -> Self {
        Self {
            duration: update_info.duration.as_millis() as u32,
            tasks: update_info.tasks as u32,
        }
    }
}

#[napi]
pub fn project_update_info_subscribe(
    #[napi(ts_arg_type = "{ __napiType: \"Project\" }")] project: External<ProjectInstance>,
    func: JsFunction,
) -> napi::Result<()> {
    let func: ThreadsafeFunction<UpdateInfo> = func.create_threadsafe_function(0, |ctx| {
        let update_info = ctx.value;
        Ok(vec![NapiUpdateInfo::from(update_info)])
    })?;
    let turbo_tasks = project.turbo_tasks.clone();
    tokio::spawn(async move {
        loop {
            let update_info = turbo_tasks
                .get_or_wait_aggregated_update_info(Duration::from_secs(1))
                .await;

            let status = func.call(Ok(update_info), ThreadsafeFunctionCallMode::NonBlocking);
            if !matches!(status, Status::Ok) {
                let error = anyhow!("Error calling JS function: {}", status);
                eprintln!("{}", error);
                break;
            }
        }
    });
    Ok(())
}
