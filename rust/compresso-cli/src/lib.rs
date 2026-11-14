//! Command-line application boundaries shared by the Compresso binary.

pub mod commands;

pub use commands::{
    AccountClient, AccountError, AccountLogin, AccountStatus, Cli, CliExitCode, CompressArgs,
    ConfigExplainArgs, ConfigModeArg, ContextArgs, DiagnosticEnvironment, DoctorArgs, ExplainArgs,
    ImpactArgs, IntegrationClientArg, IntegrationEnvironment, IntegrationMutationArgs,
    IntegrationReadArgs, MapArgs, MutationArgs, OutlineArgs, OutputArgs, ReadArgs, ReposCommand,
    ReposListArgs, ReposRemoveArgs, RootCommand, RoutingReminderArgs, SearchArgs, SetupArgs,
    SetupClientArg, SetupRuntime, StatsArgs, SystemSetupRuntime, UnavailableAccountClient,
    UnfoldArgs, execute, execute_diagnostics, execute_integrations, execute_setup,
    execute_setup_with_runtime, execute_with_store, execute_with_store_and_input,
};
