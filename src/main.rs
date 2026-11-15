mod app;
mod config;
mod dash;
mod proc;
mod theme;
mod ui;

use std::path::PathBuf;

const HELP: &str = "devhub - run, watch and manage your dev servers

Usage: devhub [--config <file>]

Options:
  -c, --config <file>   Use this config file
      --focus           Open in a maximised Windows Terminal window with no tab
                        or title bar (F11 toggles full screen, ctrl+shift+F focus mode)
      --config-path     Print the config path and exit
  -h, --help            Show this help

The config defaults to %APPDATA%\\devhub\\config.toml (or $DEVHUB_CONFIG).
A starter config is written there on first run. Press ? inside for keys.";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    let cfg_path = args
        .iter()
        .position(|a| a == "-c" || a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(config::default_path);
    if args.iter().any(|a| a == "--config-path") {
        println!("{}", cfg_path.display());
        return Ok(());
    }
    if args.iter().any(|a| a == "--focus") && open_focused(&args) {
        return Ok(());
    }

    if args.iter().all(|a| a != "-c" && a != "--config") {
        migrate_from_zdev(&cfg_path);
    }
    let mut app = app::App::new(cfg_path);
    let mut terminal = ratatui::init();
    // A crash must not leave the terminal in the theme's colours either.
    let restore_terminal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        theme::emit(theme::RESET_BACKGROUND);
        restore_terminal(info);
    }));
    let result = app.run(&mut terminal);
    if app.painted_terminal() {
        theme::emit(theme::RESET_BACKGROUND);
    }
    ratatui::restore();
    app.shutdown();
    result
}

/// Starts devhub again in a new Windows Terminal window in focus mode (no tab or title
/// bar), maximised, then lets this one end. False when Windows Terminal isn't there,
/// so devhub just runs where it was started.
fn open_focused(args: &[String]) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let rest: Vec<&String> = args.iter().filter(|a| *a != "--focus").collect();
    let started = std::process::Command::new("wt.exe")
        // `-w new`: a window of its own. With the "use existing window" setting, wt would
        // otherwise open a tab in an open window, where focus mode doesn't apply.
        .args(["-w", "new", "--focus", "--maximized", "--title", "devhub", "--suppressApplicationTitle"])
        .arg(exe)
        .args(rest)
        .spawn()
        .is_ok();
    if !started {
        eprintln!("Windows Terminal (wt.exe) not found: running here instead.");
    }
    started
}

/// The first run after the rename from zdev: copies the config, the folded sections
/// and the list of running servers from the old folders, unless devhub has its own.
fn migrate_from_zdev(cfg_path: &std::path::Path) {
    let copy = |from: PathBuf, to: PathBuf| {
        if from.exists() && !to.exists() {
            if let Some(dir) = to.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy(from, to);
        }
    };
    if let (Some(dir), Some(base)) = (cfg_path.parent(), dirs::config_dir()) {
        let old = base.join("zdev");
        copy(old.join("config.toml"), dir.join("config.toml"));
        copy(old.join("state.toml"), dir.join("state.toml"));
    }
    if let Some(local) = dirs::data_local_dir() {
        copy(local.join("zdev").join("running.toml"), proc::data_dir().join("running.toml"));
    }
}
