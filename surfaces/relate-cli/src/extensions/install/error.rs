//! The install flow's typed failure (ADR-0431 §3): the leaf module the
//! channel and unpack peers import downward — the error vocabulary
//! belongs below the flow that composes them, so the peers never reach
//! up into the flow module to name their own refusals.

/// The install's exit-code classification (ADR-0431 §3, ADR-0430 §5's
/// split), stated once on the type: [`InstallError::Usage`] is the
/// exit-2 class — the invocation or the package's manifest was
/// malformed before anything ran — and everything else
/// ([`InstallError::Runtime`]) exits 1: name-resolution, environment,
/// network, verification, unpack. The work was invited and failed. One
/// line of stderr either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallError {
    /// A manifest-or-flag problem (exit 2, ADR-0430 §5's split): the
    /// invocation or the package's manifest was malformed.
    Usage(String),
    /// A runtime failure (exit 1): the work was invited and failed —
    /// name-resolution, environment, network, verification, unpack.
    Runtime(String),
}

impl InstallError {
    /// The exact text the verb prints to stderr before the exit code.
    pub(crate) fn stderr_message(&self) -> String {
        format!("Error: extension install: {self}")
    }

    /// The process exit code (ADR-0431 §3): the classification lives on
    /// the type, so no call site decides it — and the usage(2)/runtime(1)
    /// split itself is [`crate::exit_class::ExitClass`]'s, stated once for
    /// the crate (round 4, F-2-12); this type adds only the
    /// classification and the message prefix.
    pub(crate) fn exit_code(&self) -> i32 {
        self.class().code()
    }

    /// The exit class — the projection onto
    /// [`crate::exit_class::ExitClass`].
    fn class(&self) -> crate::exit_class::ExitClass {
        match self {
            Self::Usage(_) => crate::exit_class::ExitClass::Usage,
            Self::Runtime(_) => crate::exit_class::ExitClass::Runtime,
        }
    }
}

/// The bare message text, no prefix: how a caller (the channel module's
/// `--available` path, the tests) rewraps an install failure.
impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Runtime(message) => f.write_str(message),
        }
    }
}

impl From<crate::release_channel::ChannelError> for InstallError {
    fn from(err: crate::release_channel::ChannelError) -> Self {
        InstallError::Runtime(err.to_string())
    }
}

/// The trust store's failures are runtime ones (exit 1): its typed error
/// already names the file, so the flow carries it through `?` instead of
/// re-wrapping each site by hand.
impl From<crate::extensions::trust::TrustError> for InstallError {
    fn from(err: crate::extensions::trust::TrustError) -> Self {
        InstallError::Runtime(err.to_string())
    }
}

/// The registry's failures are runtime ones too (exit 1): the load-time
/// refusals already name their offender, so the install entry point
/// carries them through `?` like the other two.
impl From<crate::extensions::registry::RegistryError> for InstallError {
    fn from(err: crate::extensions::registry::RegistryError) -> Self {
        InstallError::Runtime(err.to_string())
    }
}
