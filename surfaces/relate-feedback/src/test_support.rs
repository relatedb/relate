//! The crate's one test kit: the injectable env lookup and the recording
//! send seam every test module shares.

use std::cell::RefCell;

use crate::outcome::Reason;
use crate::report::{Category, Context, Invoker, Payload};
use crate::submit::Sender;

/// The report context every test module builds from: a labelled agent
/// invocation of one fixed version. A test that needs a different command or
/// error-code list derives it with struct update syntax.
pub(crate) fn context() -> Context {
    Context {
        cli_version: "1.0.2".to_string(),
        os: Some("darwin".to_string()),
        os_arch: Some("arm64".to_string()),
        invoker: Invoker::Agent,
        agent_harness: Some("claude-code".to_string()),
        commands: vec!["query".to_string()],
        // The CLI's nonzero exit-table code names (the fixture's stand-in;
        // the CLI passes its real table in through the same field).
        error_codes: [
            "fatal_error",
            "usage_error",
            "conflict",
            "validation_error",
            "timeout",
        ]
        .map(String::from)
        .to_vec(),
    }
}

/// An injectable env lookup built from key/value pairs: no test mutates the
/// process environment.
pub(crate) fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

/// The one report payload fixture: a clean report whose optional fields are
/// unset. A test that needs another category, invoker or command derives it
/// with struct update syntax.
pub(crate) fn payload() -> Payload {
    Payload {
        schema_version: "1".to_string(),
        surface: "cli".to_string(),
        cli_version: "1.0.2".to_string(),
        os: Some("darwin".to_string()),
        os_arch: Some("arm64".to_string()),
        invoker: Invoker::Human,
        request_id: "123e4567-e89b-42d3-a456-426614174000".to_string(),
        category: Category::Friction,
        expected: "a".to_string(),
        observed: "b".to_string(),
        command_id: None,
        error_code: None,
        agent: None,
    }
}

/// The one test send seam: records each attempt's endpoint and answers with
/// a fixed reason, so a test proves what would be sent without a network.
pub(crate) struct MockSender {
    endpoints: RefCell<Vec<String>>,
    result: Reason,
}

impl MockSender {
    pub(crate) fn new(result: Reason) -> Self {
        Self {
            endpoints: RefCell::new(Vec::new()),
            result,
        }
    }

    /// The send attempts made, derived from the recorded endpoints.
    pub(crate) fn calls(&self) -> usize {
        self.endpoints.borrow().len()
    }

    /// The endpoint the last send attempt named, if any.
    pub(crate) fn endpoint(&self) -> Option<String> {
        self.endpoints.borrow().last().cloned()
    }
}

impl Sender for MockSender {
    fn send(&self, endpoint: &str, _payload: &Payload) -> Reason {
        self.endpoints.borrow_mut().push(endpoint.to_string());
        self.result
    }
}
