use glib::ExitCode;

fn main() -> ExitCode {
    // The app icon is compiled into the binary, so it is available even when
    // lbreco runs from a build directory before installation.
    gio::resources_register_include!("lbreco.gresource")
        .expect("failed to register the bundled app resources");

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "-V" || arg == "--version") {
        println!("lbreco {}", lbreco::model::VERSION);
        return ExitCode::SUCCESS;
    }

    lbreco::ui::run()
}

fn print_help() {
    println!(
        "lbreco {version} — find your next 50 songs from your ListenBrainz history.

Usage: lbreco [OPTIONS]

Options:
  -h, --help       Show this help and exit
  -V, --version    Show the version and exit

The graphical interface takes no arguments: set your ListenBrainz username (and
optionally an OpenAI-compatible endpoint) in Preferences.",
        version = lbreco::model::VERSION
    );
}
