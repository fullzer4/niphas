use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::LogExporter;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Guard that flushes pending spans/logs on drop.
/// The caller must keep it alive until shutdown.
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tp) = self.tracer_provider.take() {
            let _ = tp.shutdown();
        }
        if let Some(lp) = self.logger_provider.take() {
            let _ = lp.shutdown();
        }
    }
}

/// Initializes tracing with optional OTEL support.
///
/// If `OTEL_EXPORTER_OTLP_ENDPOINT` is set, adds:
/// - OTLP trace exporter (spans)
/// - OTLP log exporter (logs bridge)
///
/// Without that env var, behavior is identical to before (JSON logs on stdout).
///
/// Returns a guard that MUST be kept alive in main.
pub fn init_tracing(service_name: &'static str) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = fmt::layer().json();

    let otel_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();

    if let Some(endpoint) = otel_endpoint {
        let resource = Resource::builder().with_service_name(service_name).build();

        // Trace exporter
        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
            .expect("failed to create OTLP span exporter");

        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();

        let tracer = tracer_provider.tracer(service_name);
        let otel_layer = OpenTelemetryLayer::new(tracer);

        // Log exporter
        let log_exporter = LogExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
            .expect("failed to create OTLP log exporter");

        let logger_provider = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();

        let otel_log_layer = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(
            &logger_provider,
        );

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_layer)
            .with(otel_log_layer)
            .init();

        TelemetryGuard {
            tracer_provider: Some(tracer_provider),
            logger_provider: Some(logger_provider),
        }
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();

        TelemetryGuard {
            tracer_provider: None,
            logger_provider: None,
        }
    }
}
