//! Drive Rainy's Strata manager against the real Strata release in a scratch user data folder:
//! `cargo run -p rainy-carrier --example strata_e2e -- <scratch user data> [install|catalog|check|update|start <family> <model>|stop|download <family> <model>|cancel|uninstall]...`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rainy_carrier::strata::{Choice, Manager, Phase};

fn wait(manager: &Arc<Manager>, what: &str, done: impl Fn(&rainy_carrier::strata::Status) -> bool) {
    let started = Instant::now();
    let mut last = String::new();
    loop {
        let status = manager.status();
        let line = format!(
            "job={:?} server={:?}/{:?} catalog_loading={} checking={} error={:?} notice={:?}",
            status.job.as_ref().map(|job| (job.step.clone(), job.done, job.total, job.detail.clone())),
            status.server.phase,
            status.server.health.as_ref().map(|health| (health.model.clone(), health.loaded)),
            status.catalog_loading,
            status.checking,
            status.error,
            status.notice
        );
        if line != last {
            println!("[{what} {:>5.0}s] {line}", started.elapsed().as_secs_f64());
            last = line;
        }
        if done(&status) {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn main() {
    let _ = env_logger_like();
    let mut args = std::env::args().skip(1);
    let user_data = PathBuf::from(args.next().expect("scratch user data folder"));
    let manager = Manager::new(&user_data, || {});
    manager.refresh();
    let rest: Vec<String> = args.collect();
    let mut index = 0;
    while index < rest.len() {
        let command = rest[index].as_str();
        index += 1;
        match command {
            "install" => {
                manager.install().unwrap();
                wait(&manager, "install", |status| status.job.is_none());
            }
            "catalog" => {
                manager.load_catalog();
                std::thread::sleep(Duration::from_millis(200));
                wait(&manager, "catalog", |status| !status.catalog_loading);
                let status = manager.status();
                if let Some(catalog) = &status.catalog {
                    println!("recommended: {:?}", catalog.recommendation);
                    for family in &catalog.families {
                        for size in &family.sizes {
                            println!("  {} {} {:.1} GB fit={:?} installed={}", family.id, size.model, size.download_gb, size.fit, size.installed);
                        }
                    }
                }
            }
            "check" => {
                manager.check_update().unwrap();
                std::thread::sleep(Duration::from_millis(200));
                wait(&manager, "check", |status| !status.checking);
                println!("update: {:?}", manager.status().update);
            }
            "update" => {
                manager.update().unwrap();
                wait(&manager, "update", |status| status.job.is_none());
            }
            "download" => {
                let choice = Choice { family: rest[index].clone(), model: rest[index + 1].clone() };
                index += 2;
                manager.download(choice).unwrap();
                wait(&manager, "download", |status| status.job.is_none());
            }
            "download-briefly" => {
                let choice = Choice { family: rest[index].clone(), model: rest[index + 1].clone() };
                index += 2;
                manager.download(choice).unwrap();
                let started = Instant::now();
                wait(&manager, "download", |status| status.job.is_none() || (started.elapsed() > Duration::from_secs(90) && status.job.as_ref().is_some_and(|job| job.done > 0)));
                manager.cancel();
                wait(&manager, "cancel", |status| status.job.is_none());
            }
            "start" => {
                let choice = Choice { family: rest[index].clone(), model: rest[index + 1].clone() };
                index += 2;
                manager.start(choice).unwrap();
                wait(&manager, "start", |status| status.server.phase != Phase::Starting);
            }
            "stop" => {
                manager.stop().unwrap();
                std::thread::sleep(Duration::from_millis(200));
                wait(&manager, "stop", |status| status.server.phase != Phase::Stopping);
            }
            "uninstall" => {
                manager.uninstall(true).unwrap();
                wait(&manager, "uninstall", |status| status.job.is_none());
            }
            other => panic!("unknown command {other}"),
        }
        let status = manager.status();
        println!("== after {command}: installed={:?} data_dir={} free={:?} legacy={}", status.installed, status.data_dir, status.data_free_gb, status.legacy_bytes);
    }
    manager.close();
}

/// Print the carrier's log lines to the console.
fn env_logger_like() -> Option<()> {
    struct Console;
    impl log::Log for Console {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            if record.level() <= log::Level::Info {
                eprintln!("  log {}: {}", record.level(), record.args());
            }
        }
        fn flush(&self) {}
    }
    static CONSOLE: Console = Console;
    log::set_logger(&CONSOLE).ok()?;
    log::set_max_level(log::LevelFilter::Info);
    Some(())
}
