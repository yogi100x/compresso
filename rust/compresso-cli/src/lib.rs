//! Command-line application boundaries shared by the Compresso binary.

// D117: no unsafe code (no FFI process spawning); the capability check
// requires this line.
#![forbid(unsafe_code)]

pub mod commands;
pub mod surface;

pub use commands::{
    AccountClient, AccountError, AccountLogin, AccountStatus, Cli, CliExitCode, CompressArgs,
    ConfigExplainArgs, ConfigModeArg, ContextArgs, DecisionsArgs, DiagnosticEnvironment,
    DoctorArgs, ExplainArgs, ImpactArgs, IntegrationClientArg, IntegrationEnvironment,
    IntegrationMutationArgs, IntegrationReadArgs, MapArgs, MutationArgs, OutlineArgs, OutputArgs,
    PostError, PullRequestPoster, ReadArgs, RecallArgs, RecallEnvironment, RecallSourceArg,
    ReposCommand, ReposListArgs, ReposRemoveArgs, RootCommand, RoutingReminderArgs, SearchArgs,
    SetupArgs, SetupClientArg, SetupRuntime, StatsArgs, SystemPullRequestPoster,
    SystemSetupRuntime, TraceArgs, UnavailableAccountClient, UnfoldArgs, execute,
    execute_decisions, execute_diagnostics, execute_integrations, execute_recall, execute_setup,
    execute_setup_with_runtime, execute_with_store, execute_with_store_and_input,
};
