use std::path::PathBuf;
use std::sync::OnceLock;
use async_trait::async_trait;
use channel_plugin::message::LogLevel;
use opentelemetry::{Context, trace::FutureExt};
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use serde::{Deserialize, Serialize};
use anyhow::Result;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use tracing_appender::rolling::RollingFileAppender;
use tracing_appender::rolling::Rotation;
use tracing_subscriber::fmt;
use tracing_subscriber::Registry;
use std::time::Instant;
use tracing::{info, error};
use opentelemetry::{
    global,
    trace::{TraceContextExt, Tracer},
    metrics::MeterProvider,
};

use opentelemetry_otlp::{LogExporter, MetricExporter, Protocol, SpanExporter};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider, metrics::SdkMeterProvider, trace::SdkTracerProvider,
};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;



#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LogConfig{
    pub(crate) log_level: LogLevel,
    pub(crate) log_dir: Option<PathBuf>,
    pub(crate) otel_endpoint: Option<String>,
}

impl LogConfig{
    pub fn new(log_level: LogLevel, log_dir: Option<PathBuf>, otel_endpoint: Option<String>) -> Self {
        Self{log_level,log_dir,otel_endpoint}
    }
    pub fn default() -> Self {
        Self{log_level:LogLevel::Info,log_dir:None,otel_endpoint:None}
    }
}

#[async_trait]
#[typetag::serde]
pub trait LoggerType: Send + Sync {
    fn log(&self, level: LogLevel, context: &str, msg: &str);
    fn clone_box(&self) -> Box<dyn LoggerType>;
    fn debug_box(&self) -> String;
}

#[derive(Serialize, Deserialize)]
pub struct Logger(pub Box<dyn LoggerType>);

impl Logger {
    pub fn into_inner(self) -> Box<dyn LoggerType> {
        self.0
    }
}

impl Clone for Logger {
    fn clone(&self) -> Self {
        Logger(self.0.clone_box())
    }
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.debug_box())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, )]
pub struct OpenTelemetryLogger;

impl OpenTelemetryLogger {
    pub fn new() -> Self {
       Self
    }
}

#[typetag::serde]
#[async_trait]
impl LoggerType for OpenTelemetryLogger {
fn log(&self, level: LogLevel, context: &str, msg: &str) {
        // Add tracing-level logs
        match level {
            LogLevel::Trace => tracing::trace!(%context, "{msg}"),
            LogLevel::Debug => tracing::debug!(%context, "{msg}"),
            LogLevel::Info => tracing::info!(%context, "{msg}"),
            LogLevel::Warn => tracing::warn!(%context, "{msg}"),
            LogLevel::Error => tracing::error!(%context, "{msg}"),
            LogLevel::Critical => tracing::error!(%context, "[CRITICAL] {msg}"),
        }
    }

    fn clone_box(&self) -> Box<dyn LoggerType> {
        Box::new(self.clone())
    }

    fn debug_box(&self) -> String {
        "OpenTelemetryLogger".to_string()
    }
}

pub fn init_tracing(
    root: PathBuf,
    log_file: String,
    event_file: String,
    log_level: String,
    otel_logs_endpoint: Option<String>,
    otel_events_endpoint: Option<String>,
) -> anyhow::Result<Logger> {
    let otel_enabled = otel_logs_endpoint.is_some() || otel_events_endpoint.is_some();

    if otel_enabled {
        // wire up OTLP‐HTTP for logs, traces & metrics
        let _telemetry = Telemetry::init(
            &log_level,
            otel_logs_endpoint.as_deref().unwrap_or_default(),
            /* tracer_endpoint */ otel_logs_endpoint.as_deref().unwrap_or_default(),
            /* meter_endpoint  */ otel_events_endpoint.as_deref().unwrap_or_default(),
        );
    } else {
        // wire up file‐based logs + JSON reports
        let _file_telem = FileTelemetry::init_files(
            &log_level,
            root.join(&log_file),
            root.join(&event_file),
        )?;
    }

    // The `Logger` itself just wraps the tracing‐based logger impl:
    Ok(Logger(Box::new(OpenTelemetryLogger::new())))
}

static RESOURCE: OnceLock<Resource> = OnceLock::new();
fn get_resource() -> Resource {
    RESOURCE.get_or_init(|| {
        Resource::builder()
            .with_service_name("greentic-service")
            .build()
    }).clone()
}

/// Initialize the three OTLP‐HTTP providers
fn init_logs(end_point: &str) -> SdkLoggerProvider {
    let exporter = LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(end_point)
        .build()
        .expect("log‐exporter");
    SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(get_resource())
        .build()
}

fn init_traces(end_point: &str) -> SdkTracerProvider {
    let exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(end_point)
        .build()
        .expect("span‐exporter");
    SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(get_resource())
        .build()
}

fn init_metrics(end_point: &str) -> SdkMeterProvider {
    let exporter = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(end_point)
        .build()
        .expect("metric‐exporter");
 
    SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(get_resource())
        .build()
}

/// Bundles everything you need at runtime
pub struct Telemetry {
    /// emits OTel Logs from `tracing::event!`
    pub logger_provider: SdkLoggerProvider,
    /// emits OTel Spans from `tracer.start()`
    pub tracer_provider: SdkTracerProvider,
    /// emits OTel Metrics from counters/histograms
    pub meter_provider: SdkMeterProvider,

    /// application‐level counters & histograms
    pub requests_started: Counter<u64>,
    pub requests_succeeded: Counter<u64>,
    pub requests_failed: Counter<u64>,
    pub request_latency_ms: Histogram<f64>,
}

impl Telemetry {
    pub fn init(log_level: &str, logger_endpoint: &str, tracer_endpoint: &str, meter_endpoint: &str) -> Self {
        // 1) bring up the three SDKs
        let logger_provider = init_logs(logger_endpoint);
        let tracer_provider = init_traces(tracer_endpoint);
        let meter_provider = init_metrics(meter_endpoint);

        // 2) build the OTLP‐Tracing bridge layer
        let otel_logs_layer = {
            let filter = EnvFilter::new(log_level)
                .add_directive("hyper=off".parse().unwrap())
                .add_directive("tonic=off".parse().unwrap())
                .add_directive("h2=off".parse().unwrap())
                .add_directive("reqwest=off".parse().unwrap());
            OpenTelemetryTracingBridge::new(&logger_provider)
                .with_filter(filter)
        };

        // 3) a local pretty‐printer so you still see `info!` on stdout
        let fmt_layer = fmt::layer()
            .with_thread_names(true)
            .with_filter(EnvFilter::new(log_level).add_directive("opentelemetry=debug".parse().unwrap()));

        // 4) install subscriber
        Registry::default()
            .with(otel_logs_layer)
            .with(fmt_layer)
            .init();

        // 5) register the tracer & meter globally (for `global::tracer()` etc.)
        //let tracer = tracer_provider.tracer("greentic-service");
        global::set_tracer_provider(tracer_provider.clone());
        let meter = meter_provider.meter("greentic-service");
        global::set_meter_provider(meter_provider.clone());

        // 6) create your application metrics once
        let requests_started = meter
            .u64_counter("requests_started")
            .with_description("Total requests started")
            .build();
        let requests_succeeded = meter
            .u64_counter("requests_succeeded")
            .build();
        let requests_failed = meter
            .u64_counter("requests_failed")
            .build();
        let request_latency_ms = meter
            .f64_histogram("request_latency_ms")
            .with_description("Latency per request in ms")
            .with_unit("ms")
            .build();

        Telemetry {
            logger_provider,
            tracer_provider,
            meter_provider,
            requests_started,
            requests_succeeded,
            requests_failed,
            request_latency_ms,
        }
    }

    /// Wrap any async request‐handler with logs + tracing + metrics
    pub async fn instrument_request<F, Fut, T>(&self, name: &str, handler: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // start metrics & span
        self.requests_started.add(1, &[]);
        let start = Instant::now();
        let span = self.tracer_provider.tracer("greentic-service")
            .start(name.to_string());

        // Build a new Context that has `span` as the active span
        let cx: Context = Context::current_with_span(span);
        let res = handler().with_context(cx).await;
        // record latency
        let elapsed = start.elapsed().as_secs_f64() * 1_000.0;
        self.request_latency_ms.record(elapsed, &[]);

        // log & metrics based on outcome
        info!(target: "request", "request `{}` completed in {} ms", name, elapsed);
        self.requests_succeeded.add(1, &[]);

        res
    }
}


/// Bundles everything you need at runtime, but writing *only* to files.
pub struct FileTelemetry {
    /// application‐level counters & histograms
    pub requests_started: Counter<u64>,
    pub requests_succeeded: Counter<u64>,
    pub requests_failed: Counter<u64>,
    pub request_latency_ms: Histogram<f64>,
}

impl FileTelemetry {
    /// Initialize file‐based logging + metrics.
    ///
    /// - `log_level` is an `EnvFilter` directive (e.g. `"info"`).
    /// - `log_file` is the path to your rolling text log.
    /// - `event_file` is the path to your rolling JSON “report” log.
    pub fn init_files(
        log_level: &str,
        log_file: PathBuf,
        event_file: PathBuf,
    ) -> Result<Self> {
        // 1) Build an EnvFilter
        let env_filter = EnvFilter::new(log_level);

        // 2) A plain‐text rolling file appender for info!/error! logs
        let txt_appender = RollingFileAppender::new(Rotation::DAILY, log_file.parent().unwrap(), log_file.file_name().unwrap());
        let txt_layer = fmt::Layer::default()
            .with_writer(txt_appender)
            .with_ansi(false);
            //.with_filter(env_filter);

        // 3) A JSON‐formatter rolling appender for request‐reports
        let json_appender = RollingFileAppender::new(Rotation::DAILY, event_file.parent().unwrap(), event_file.file_name().unwrap());
        let json_layer = fmt::layer()
            .json() // newline‐delimited JSON
            .with_writer(json_appender)
            .with_target(true)                 // emit only events with target="request"
            .with_filter(EnvFilter::new("request=info"));
            //.with_filter(env_filter);

        // 4) Install subscriber
        Registry::default()
            .with(env_filter)
            .with(txt_layer)
            .with(json_layer)
            .init();

        // 5) Set up a global meter (using the no‐op provider behind the scenes)
        //    so our Counters/Histograms still compile and record to memory.
        let meter = global::meter("file-telemetry");

        // 6) Create your application metrics once
        let requests_started = meter
            .u64_counter("requests_started")
            .with_description("Total requests started")
            .build();
        let requests_succeeded = meter
            .u64_counter("requests_succeeded")
            .build();
        let requests_failed = meter
            .u64_counter("requests_failed")
            .build();
        let request_latency_ms = meter
            .f64_histogram("request_latency_ms")
            .with_description("Latency per request in ms")
            .with_unit("ms")
            .build();

        Ok(FileTelemetry {
            requests_started,
            requests_succeeded,
            requests_failed,
            request_latency_ms,
        })
    }

    /// Wrap any async “request” with logs + metrics, writing to files.
    ///
    /// Any `info!`/`error!` inside `handler` will go to the text log.
    /// At the end we emit one JSON line (target = "request") with name+latency.
    pub async fn instrument_request<F, Fut, T, E>(
        &self,
        name: &str,
        handler: F,
    ) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        // 1) start metrics & timer
        self.requests_started.add(1, &[]);
        let start = Instant::now();

        // 2) run the handler
        let result = handler().await;

        // 3) record latency
        let elapsed = start.elapsed().as_secs_f64() * 1_000.0;
        self.request_latency_ms.record(elapsed, &[]);

        // 4) metrics + logs + JSON event
        match &result {
            Ok(_) => {
                self.requests_succeeded.add(1, &[]);
                info!("request `{}` succeeded in {} ms", name, elapsed);
            }
            Err(err) => {
                self.requests_failed.add(1, &[]);
                error!(error = %err, "request `{}` failed in {} ms", name, elapsed);
            }
        }

        // 5) JSON “request” event line
        //    (picked up by json_layer above, since we use target="request")
        tracing::event!(
            target: "request",
            tracing::Level::INFO,
            request = name,
            latency_ms = elapsed,
            status = match &result {
                Ok(_) => "ok",
                Err(_) => "error",
            },
        );

        result
    }
}