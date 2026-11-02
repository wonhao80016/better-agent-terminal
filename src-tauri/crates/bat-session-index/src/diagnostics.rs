use std::sync::OnceLock;

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Routes this crate's non-fatal diagnostics to the host log. Only the first call takes effect; until then they
/// go to stderr. Messages never contain session ids, paths or transcript contents.
pub fn set_diagnostic_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn diagnostic(message: &str) {
    match SINK.get() {
        Some(sink) => sink(message),
        None => eprintln!("[bat-session-index] {message}"),
    }
}
