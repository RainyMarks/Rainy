use ::core::prelude::v1::test;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::lifecycle::local_choice;
use super::storage::downloaded_bytes;
use super::*;

#[test]
fn settings_default_and_round_trip() {
    let settings = Settings::default();
    assert_eq!((settings.port, settings.auto_update, settings.source), (8080, true, Source::Huggingface));
    let text = serde_json::to_string(&Settings { selected: Some(Choice { family: "qwen".into(), model: "Q2_0".into() }), source: Source::Modelscope, ..Settings::default() }).unwrap();
    assert!(text.contains("\"source\":\"modelscope\"") && text.contains("\"autoUpdate\":true"));
    let read: Settings = serde_json::from_str(&text).unwrap();
    assert_eq!(read.selected.unwrap().model, "Q2_0");
    // A file of a later version with more fields still reads.
    let later: Settings = serde_json::from_str(r#"{"port": 8099, "somethingNew": 1}"#).unwrap();
    assert_eq!(later.port, 8099);
}

#[test]
fn uses_a_strata_the_person_installed_but_never_changes_it() {
    let dir = tempfile::tempdir().unwrap();
    let own = dir.path().join("own");
    let terminal = own.join("terminal").join("0.2.8");
    for folder in ["python", "strata_control", "tools"] {
        fs::create_dir_all(terminal.join(folder)).unwrap();
    }
    for file in ["python/python.exe", "strata.py", "tools/strata_mcp.py"] {
        fs::write(terminal.join(file), b"").unwrap();
    }
    let manager = Manager::with_own(&dir.path().join("rainy"), Some(own.clone()), || {});
    let status = manager.status();
    let installed = status.installed.unwrap();
    assert_eq!((installed.control.as_str(), installed.external.as_deref()), ("0.2.8", Some(terminal.to_str().unwrap())));
    // Its own folder holds its models unless a folder is chosen here; Rainy neither updates nor removes it.
    assert_eq!(status.data_dir, "");
    assert!(manager.update().is_err() && manager.uninstall(false).is_err());
    assert!(terminal.join("strata.py").is_file());
}

#[test]
fn recognizes_model_files_the_person_has() {
    let found = serde_json::json!({"schema_version": 1, "ok": true, "plan": {"model_dir": "E:\\models\\gguf", "candidate_selection": {"family": "qwen", "model": "Q2_0"}}});
    let (choice, folder) = local_choice(&found, "E:\\models").unwrap();
    assert_eq!((choice.family.as_str(), choice.model.as_str(), folder), ("qwen", "Q2_0", PathBuf::from("E:\\models\\gguf")));
    let unknown = serde_json::json!({"schema_version": 1, "ok": false, "plan": {"candidate_selection": null, "plain_issues": ["These files are not a model Strata knows."]}});
    assert!(local_choice(&unknown, "E:\\x").unwrap_err().to_string().contains("not a model Strata knows"));
}

#[test]
fn a_fresh_manager_offers_the_download() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::with_own(dir.path(), None, || {});
    let status = manager.status();
    assert!(status.installed.is_none() && status.job.is_none());
    assert_eq!(status.server.phase, Phase::Stopped);
    assert!(manager.start(Choice { family: "qwen".into(), model: "Q2_0".into() }).is_err());
    assert!(manager.save(Settings { port: 80, ..Settings::default() }).is_err());
    let saved = manager.save(Settings { port: 18080, ..Settings::default() }).unwrap();
    assert_eq!(saved.settings.port, 18080);
    assert_eq!(Manager::new(dir.path(), || {}).status().settings.port, 18080);
}

#[test]
fn a_data_folder_must_be_named_for_strata() {
    assert_eq!(Manager::data_folder_for(Path::new("D:\\Strata-data")), PathBuf::from("D:\\Strata-data"));
    assert_eq!(Manager::data_folder_for(Path::new("D:\\models")), PathBuf::from("D:\\models\\Strata-data"));
    assert_eq!(Manager::data_folder_for(Path::new("E:\\strata")), PathBuf::from("E:\\strata"));
    // Choosing the models folder, or one model's folder, of a data folder means that data folder.
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("Strata-data");
    fs::create_dir_all(data.join("models").join("Q2_0")).unwrap();
    assert_eq!(Manager::data_folder_for(&data.join("models")), data);
    assert_eq!(Manager::data_folder_for(&data.join("models").join("Q2_0")), data);
    assert_eq!(Manager::data_folder_for(&data), data);
}

/// A Strata Terminal Rainy downloaded, enough for `Manager::control`.
fn fake_terminal(user_data: &Path) {
    let terminal = user_data.join("tools").join("strata").join("control").join("0.1.57");
    for folder in ["python", "strata_control", "tools"] {
        fs::create_dir_all(terminal.join(folder)).unwrap();
    }
    for file in ["python/python.exe", "strata.py", "tools/strata_mcp.py"] {
        fs::write(terminal.join(file), b"").unwrap();
    }
}

#[test]
fn the_data_folder_stays_where_strata_put_the_models() {
    let dir = tempfile::tempdir().unwrap();
    let user_data = dir.path().join("rainy");
    fake_terminal(&user_data);
    let folders = Folders::new(&user_data);
    // Strata's setup remembers the folder of the last download; Rainy keeps using it, whatever drive has room now.
    let remembered = dir.path().join("Strata-data");
    fs::create_dir_all(&remembered).unwrap();
    fs::create_dir_all(folders.roaming.join("Strata")).unwrap();
    fs::write(folders.roaming.join("Strata").join("settings.json"), serde_json::to_string(&json!({ "data_dir": remembered })).unwrap()).unwrap();
    let manager = Manager::with_own(&user_data, None, || {});
    assert_eq!(manager.status().data_dir, remembered.to_string_lossy());
    // Once a download starts the folder is saved, so it is the same after Strata's record is gone.
    manager.keep_data_dir();
    fs::remove_file(folders.roaming.join("Strata").join("settings.json")).unwrap();
    let again = Manager::with_own(&user_data, None, || {});
    assert_eq!(again.status().data_dir, remembered.to_string_lossy());
}

#[test]
fn finds_model_files_strata_has_not_set_up() {
    let dir = tempfile::tempdir().unwrap();
    let user_data = dir.path().join("rainy");
    fake_terminal(&user_data);
    let folders = Folders::new(&user_data);
    let data = folders.root.join("Strata-data");
    // A finished download: every GGUF has its finish mark.
    let done = data.join("models").join("Q2_0");
    fs::create_dir_all(&done).unwrap();
    for name in ["Q2_0-00001-of-00002.gguf", "Q2_0-00002-of-00002.gguf"] {
        fs::write(done.join(name), vec![0u8; 100]).unwrap();
        fs::write(done.join(format!("{name}.done")), b"").unwrap();
    }
    // One that stopped half-way.
    let half = data.join("models").join("IQ2_XS");
    fs::create_dir_all(&half).unwrap();
    fs::write(half.join("IQ2_XS-00001-of-00003.gguf.part"), vec![0u8; 40]).unwrap();
    // One Strata set up already (its run config exists): not offered again.
    let set_up = data.join("models").join("IQ3_S");
    fs::create_dir_all(&set_up).unwrap();
    fs::write(set_up.join("IQ3_S-00001-of-00001.gguf"), b"x").unwrap();
    fs::create_dir_all(&folders.install).unwrap();
    fs::write(folders.install.join("strata-iq3_s.json"), b"{}").unwrap();
    let manager = Manager::with_own(&user_data, None, || {});
    // No other data folder holds models in this test, so the one with them is the automatic folder.
    assert_eq!(manager.status().data_dir, data.to_string_lossy());
    manager.refresh();
    let found = manager.status().found;
    let summary: Vec<(&str, bool, u64)> = found.iter().map(|found| (found.id.as_str(), found.complete, found.bytes)).collect();
    assert!(summary.contains(&("q2_0", true, 200)) && summary.contains(&("iq2_xs", false, 40)), "{summary:?}");
    assert!(!summary.iter().any(|(id, _, _)| *id == "iq3_s"));
    assert!(found.iter().all(|found| found.data_dir == data.to_string_lossy()));
}

#[test]
fn the_download_source_follows_the_interface_until_chosen() {
    let automatic = Settings::default();
    crate::locale::set_chinese(true);
    assert_eq!(automatic.effective_source(), Source::Modelscope);
    let chosen = Settings { source: Source::Huggingface, source_chosen: true, ..Settings::default() };
    assert_eq!(chosen.effective_source(), Source::Huggingface);
    // A file of 4.8 or earlier saved Hugging Face without anyone choosing it.
    let earlier: Settings = serde_json::from_str(r#"{"port": 8080, "source": "huggingface"}"#).unwrap();
    assert_eq!(earlier.effective_source(), Source::Modelscope);
}

#[test]
fn counts_the_files_earlier_versions_left() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("modules").join("strata").join("engine")).unwrap();
    fs::write(dir.path().join("modules").join("strata").join("engine").join("strata.exe"), vec![0u8; 1000]).unwrap();
    fs::create_dir_all(dir.path().join("strata").join("packs")).unwrap();
    fs::write(dir.path().join("strata").join("settings.json"), b"{}").unwrap();
    let manager = Manager::new(dir.path(), || {});
    manager.refresh();
    assert_eq!(manager.status().legacy_bytes, 1002);
    manager.remove_legacy().unwrap();
    assert_eq!(manager.status().legacy_bytes, 0);
    assert!(!dir.path().join("modules").join("strata").exists() && !dir.path().join("strata").exists());
}

#[test]
fn counts_the_finished_pieces_of_an_unfinished_download() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("m.gguf.part");
    let file = fs::File::create(&part).unwrap();
    file.set_len(60).unwrap();
    drop(file);
    // Without a list the file is one run from the start.
    assert_eq!(downloaded_bytes(&part), 60);
    fs::write(
        dir.path().join("m.gguf.part.chunks"),
        r#"{"v": 1, "total": 95, "chunk": 10, "done": [[0, 3], [5, 7], [9, 10]]}"#,
    )
    .unwrap();
    // Pieces 0-2 and 5; piece 6 and the last one (90-95) reach past what the file holds.
    assert_eq!(downloaded_bytes(&part), 40);
    let file = fs::OpenOptions::new().write(true).open(&part).unwrap();
    file.set_len(95).unwrap();
    drop(file);
    assert_eq!(downloaded_bytes(&part), 30 + 20 + 5);
    // Overlapping intervals and enormous malformed ranges finish instantly in O(1) per interval without overflow.
    fs::write(
        dir.path().join("m.gguf.part.chunks"),
        r#"{"v": 1, "total": 18446744073709551615, "chunk": 1, "done": [[0, 18446744073709551615], [0, 50], [10, 95]]}"#,
    )
    .unwrap();
    let started = Instant::now();
    assert_eq!(downloaded_bytes(&part), 95);
    assert!(started.elapsed() < Duration::from_secs(1));
    fs::write(dir.path().join("m.gguf.part.chunks"), "not json").unwrap();
    assert_eq!(downloaded_bytes(&part), 95);
    assert_eq!(downloaded_bytes(&dir.path().join("missing.gguf")), 0);
}

#[test]
fn uninstalled_refresh_stays_lightweight_and_does_not_probe_loopback() {
    // Bind a quiet listener on a random port that never responds; if refresh probed it when uninstalled, it would stall.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::with_own(dir.path(), None, || {});
    manager.save(Settings { port, ..Settings::default() }).unwrap();
    let started = Instant::now();
    manager.refresh();
    assert!(started.elapsed() < Duration::from_secs(1), "uninstalled refresh should not probe port: {:?}", started.elapsed());
    assert_eq!(manager.status().server.phase, Phase::Stopped);
}

/// The real thing, run by hand: Strata's Terminal downloaded, the smallest model of the catalog downloaded to
/// RAINY_QA_STRATA_GB gigabytes (3) and cancelled, then a new manager, as after a restart, finds the unfinished
/// files and a second download continues from them rather than from zero. Everything goes to RAINY_QA_STRATA (a
/// scratch folder: user data and the data folder), which is removed at the end unless RAINY_QA_STRATA_KEEP is set.
/// `cargo test -p rainy-carrier real_partial -- --ignored --nocapture`
#[test]
#[ignore]
fn real_partial_download_is_found_and_resumed() {
    let scratch = PathBuf::from(std::env::var("RAINY_QA_STRATA").expect("RAINY_QA_STRATA"));
    let stop_at = (std::env::var("RAINY_QA_STRATA_GB")
        .ok()
        .and_then(|gb| gb.parse::<f64>().ok())
        .unwrap_or(3.0)
        * 1e9) as u64;
    let (user_data, data) = (scratch.join("ud"), scratch.join("Strata-data"));
    let say = |text: String| println!("[{:>6.1} s] {text}", START.elapsed().as_secs_f64());
    static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    let wait = |what: &str, limit: Duration, mut done: Box<dyn FnMut() -> bool + '_>| {
        let until = Instant::now() + limit;
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    let open = |label: &str| {
        let manager = Manager::new(&user_data, || {});
        let mut settings = manager.status().settings;
        (settings.data_dir, settings.auto_update) = (Some(data.to_string_lossy().into_owned()), false);
        manager.save(settings).unwrap();
        manager.refresh();
        if manager.status().installed.is_none_or(|installed| installed.engine.is_none()) {
            say(format!("{label}: installing Strata"));
            manager.install().unwrap();
            wait("the installation", Duration::from_secs(60 * 60), Box::new(|| manager.status().job.is_none()));
            let status = manager.status();
            assert_eq!(status.error, None);
            say(format!("installed: {:?}", status.installed));
        }
        manager.load_catalog();
        wait(
            "the catalog",
            Duration::from_secs(600),
            Box::new(|| {
                let status = manager.status();
                assert_eq!(status.catalog_error, None);
                status.catalog.is_some()
            }),
        );
        manager
    };
    let manager = open("first start");
    let catalog = manager.status().catalog.unwrap();
    let (family, size) = catalog
        .families
        .iter()
        .flat_map(|family| family.sizes.iter().map(move |size| (family, size)))
        .min_by(|a, b| a.1.download_gb.total_cmp(&b.1.download_gb))
        .unwrap();
    let choice = Choice { family: family.id.clone(), model: size.model.clone() };
    say(format!("smallest model: {} {} ({:.1} GB, id {})", choice.family, choice.model, size.download_gb, size.id));
    let id = size.id.to_lowercase();
    manager.download(choice.clone()).unwrap();
    let mut last = 0;
    wait(
        "the first download",
        Duration::from_secs(3 * 60 * 60),
        Box::new(|| {
            let status = manager.status();
            assert_eq!(status.error, None);
            let job = status.job.expect("the download ended early");
            if job.done / 500_000_000 != last / 500_000_000 {
                say(format!("{} {:.2} / {:.2} GB", job.step, job.done as f64 / 1e9, job.total as f64 / 1e9));
            }
            last = job.done;
            job.done >= stop_at
        }),
    );
    let cancelled_at = last;
    say("cancelling".into());
    manager.cancel();
    wait("the cancel", Duration::from_secs(120), Box::new(|| manager.status().job.is_none()));
    manager.close();
    drop(manager);

    let again = open("second start");
    let found = again.status().found.into_iter().find(|found| found.id == id).expect("the unfinished download is not found");
    say(format!("found {} in {}: {:.2} GB, complete {}", found.id, found.data_dir, found.bytes as f64 / 1e9, found.complete));
    assert!(!found.complete);
    assert!(found.bytes <= cancelled_at + 100_000_000 && found.bytes as f64 >= stop_at as f64 * 0.6, "{} bytes found, {cancelled_at} downloaded", found.bytes);
    assert_eq!(PathBuf::from(&found.data_dir), data);
    again.download(choice).unwrap();
    let mut first: Option<u64> = None;
    wait(
        "the resumed download",
        Duration::from_secs(60 * 60),
        Box::new(|| {
            let status = again.status();
            assert_eq!(status.error, None);
            let job = status.job.expect("the resumed download ended early");
            if job.step == "model:download" && job.done > 0 {
                let start = *first.get_or_insert_with(|| {
                    say(format!("resumed at {:.2} GB", job.done as f64 / 1e9));
                    job.done
                });
                return job.done >= start + 300_000_000;
            }
            false
        }),
    );
    let start = first.unwrap();
    assert!(start as f64 >= found.bytes as f64 * 0.9, "the download started over: at {start} bytes, {} found", found.bytes);
    say(format!("went on to {:.2} GB; cancelling", again.status().job.map_or(0, |job| job.done) as f64 / 1e9));
    again.cancel();
    wait("the second cancel", Duration::from_secs(120), Box::new(|| again.status().job.is_none()));
    again.close();
    drop(again);
    if std::env::var_os("RAINY_QA_STRATA_KEEP").is_none() {
        std::fs::remove_dir_all(&scratch).unwrap();
        say(format!("removed {}", scratch.display()));
    }
}
