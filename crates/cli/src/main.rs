mod commands;

use clap::{ColorChoice, CommandFactory, FromArgMatches};
use commands::{audit, coverage, limits, output::Output, Cli, Commands};

fn main() {
    let mut command = Cli::command();
    if commands::output::no_color_requested(std::env::args_os()) {
        command = command.color(ColorChoice::Never);
    }
    let cli = Cli::from_arg_matches(&command.get_matches()).unwrap_or_else(|err| err.exit());
    let out = Output::from_args(&cli.global);
    let result = match cli.command {
        Commands::Coverage(args) => coverage::run(args, out),
        Commands::Limits(args) => limits::run(args, out),
        Commands::Audit(args) => audit::run(args, out),
        Commands::LimitsProbe(args) => match limits::run_probe(args) {
            Ok(true) => Ok(()),
            Ok(false) => std::process::exit(limits::PROBE_REJECTED_EXIT_CODE),
            Err(err) => Err(err),
        },
    };
    if let Err(err) = result {
        eprintln!("{}", commands::render_error(out.error_prefix(), &err));
        std::process::exit(1);
    }
}
