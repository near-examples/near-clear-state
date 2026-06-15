//! `near-clear-state` binary entry point.
//!
//! Invoked via near-cli-rs's PATH dispatch — typing `near clear-state <args>`
//! strips the `clear-state` token and runs this binary with the remaining
//! args, so we accept account_id / max_calls / network subcommand chain
//! directly at the top level.

use color_eyre::owo_colors::OwoColorize;
use interactive_clap::ToCliArgs;
use near_clear_state::ClearStateCommand;
use near_cli_rs::{CliResult, Verbosity};

fn main() -> CliResult {
    inquire::set_global_render_config(near_cli_rs::get_global_render_config());

    let config = near_cli_rs::config::Config::get_config_toml()?;

    #[cfg(not(debug_assertions))]
    let display_env_section = false;
    #[cfg(debug_assertions)]
    let display_env_section = true;
    color_eyre::config::HookBuilder::default()
        .display_env_section(display_env_section)
        .install()?;

    let cli = match ClearStateCommand::try_parse() {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };

    near_cli_rs::setup_tracing(Verbosity::Interactive)?;

    let global_context = near_cli_rs::GlobalContext {
        config,
        offline: false,
        verbosity: Verbosity::Interactive,
    };

    match <ClearStateCommand as interactive_clap::FromCli>::from_cli(Some(cli), global_context) {
        interactive_clap::ResultFromCli::Ok(cli_cmd)
        | interactive_clap::ResultFromCli::Cancel(Some(cli_cmd)) => {
            eprintln!(
                "Your console command:\n{} {}",
                std::env::args()
                    .next()
                    .as_deref()
                    .unwrap_or("near-clear-state")
                    .green(),
                shell_words::join(cli_cmd.to_cli_args()).green(),
            );
            Ok(())
        }
        interactive_clap::ResultFromCli::Cancel(None) => {
            eprintln!("Goodbye!");
            Ok(())
        }
        interactive_clap::ResultFromCli::Back => {
            unreachable!("top-level command has no `back` option")
        }
        interactive_clap::ResultFromCli::Err(_, err) => Err(err),
    }
}
