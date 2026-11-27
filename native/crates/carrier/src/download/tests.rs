use ::core::prelude::v1::test;

use super::parallel::{classify_worker_errors, pieces};
use super::url::Address;
use super::*;

fn options() -> Options {
    Options { allow_loopback_http: true, ..Options::new(Arc::new(AtomicBool::new(false))) }
}

fn sha256(bytes: &[u8]) -> Digest {
    let mut sink = HashSink::new(&Digest::Sha256([0; 32]));
    sink.update(bytes);
    sink.finish()
}

#[test]
fn reads_digests() {
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(Digest::sha256_hex(empty), Some(sha256(b"")));
    assert_eq!(Digest::sha256_hex(&empty.to_uppercase()), None);
    assert_eq!(Digest::sha256_hex(&empty[1..]), None);
    let abc = "3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==";
    let mut sink = HashSink::new(&Digest::Sha512([0; 64]));
    sink.update(b"abc");
    assert_eq!(Digest::sha512_base64(abc), Some(sink.finish()));
    assert_eq!(Digest::sha512_base64("abc"), None);
}

#[test]
fn accepts_only_secure_addresses() {
    let strict = Options::new(Arc::new(AtomicBool::new(false)));
    let address = Address::parse("https://GitHub.com/RainyMarks/Rainy/releases/download/v1.0.0-resources/x.001#frag", &strict).unwrap();
    assert_eq!((address.secure, address.host.as_str(), address.port), (true, "github.com", 443));
    assert_eq!(address.path, "/RainyMarks/Rainy/releases/download/v1.0.0-resources/x.001");
    assert_eq!(Address::parse("https://example.com:8443?x=1", &strict).unwrap().path, "/?x=1");
    for refused in [
        "http://example.com/x",
        "http://127.0.0.1/x",
        "ftp://example.com/x",
        "https://user:pw@example.com/",
        "https:///x",
        "https://a b/",
        "example.com",
    ] {
        assert_eq!(Address::parse(refused, &strict), Err(DownloadError::Insecure(refused.to_owned())), "{refused}");
    }
    let loopback = options();
    for allowed in ["http://127.0.0.1:8080/a", "http://localhost/a", "http://[::1]:9/a"] {
        assert!(Address::parse(allowed, &loopback).is_ok(), "{allowed}");
    }
    assert!(Address::parse("http://example.com/x", &loopback).is_err());
    assert!(Address::parse("http://127.0.0.2/x", &loopback).is_err());
}

#[test]
fn resolves_redirect_locations() {
    let base = Address::parse("https://github.com/a/b/file.001?x=1", &options()).unwrap();
    assert_eq!(base.join("https://objects.example.com/z?sig=1"), "https://objects.example.com/z?sig=1");
    assert_eq!(base.join("//cdn.example.com/z"), "https://cdn.example.com/z");
    assert_eq!(base.join("/root"), "https://github.com/root");
    assert_eq!(base.join("other.002"), "https://github.com/a/b/other.002");
    assert_eq!(base.join("?y=2"), "https://github.com/a/b/file.001?y=2");
    assert_eq!(base.join("path/with://inside"), "https://github.com/a/b/path/with://inside");
}

#[test]
fn names_partial_files_and_strips_piece_suffixes() {
    assert_eq!(partial_path(Path::new("C:/m/.downloads/php.tar.gz")), PathBuf::from("C:/m/.downloads/php.tar.gz.partial"));
    assert_eq!(strip_partial_suffix("php.tar.gz.partial"), Some("php.tar.gz"));
    assert_eq!(strip_partial_suffix("php.tar.gz.partial.0"), Some("php.tar.gz"));
    assert_eq!(strip_partial_suffix("php.tar.gz.partial.15"), Some("php.tar.gz"));
    assert_eq!(strip_partial_suffix("php.tar.gz.partial."), None);
    assert_eq!(strip_partial_suffix("php.tar.gz.partial.bak"), None);
    assert_eq!(strip_partial_suffix(".partial"), None);
    assert_eq!(strip_partial_suffix(".partial.0"), None);
}

#[test]
fn pieces_handles_huge_u64_without_overflow_and_prioritizes_range_errors() {
    let huge = u64::MAX - 10;
    let ranges = pieces(huge, 6);
    assert_eq!(ranges.len(), 6);
    assert_eq!(ranges[0].0, 0);
    assert_eq!(ranges.last().unwrap().1, huge);
    for pair in ranges.windows(2) {
        assert_eq!(pair[0].1, pair[1].0);
    }

    let mixed = vec![
        Err(DownloadError::Interrupted),
        Err(DownloadError::Cancelled),
        Err(DownloadError::InvalidResume),
    ];
    assert_eq!(classify_worker_errors(&mixed), Ok(false));
    let network_only = vec![Err(DownloadError::Cancelled), Err(DownloadError::Network("reset".into()))];
    assert_eq!(classify_worker_errors(&network_only), Err(DownloadError::Network("reset".into())));
}

#[test]
fn refuses_hard_linked_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, b"x").unwrap();
    assert_eq!(check_path(&file), Ok(()));
    assert_eq!(check_path(&dir.path().join("missing").join("deeper")), Ok(()));
    fs::hard_link(&file, dir.path().join("second")).unwrap();
    assert!(matches!(check_path(&file), Err(DownloadError::Link(_))));
    assert!(matches!(check_path(&file.join("below")), Err(DownloadError::Link(_))));
}

#[cfg(windows)]
mod network {
    use ::core::prelude::v1::test;
    use std::sync::Mutex;
    use std::time::Instant;

    use super::super::parallel::{piece_path, pieces};
    use super::super::*;
    use super::{options, sha256};
    use crate::test_server::{Reply, Server};

    fn body(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index * 7 % 251) as u8).collect()
    }

    fn expected(bytes: &[u8]) -> Expected {
        Expected { bytes: bytes.len() as u64, digest: sha256(bytes) }
    }

    #[test]
    fn downloads_a_whole_file() {
        let data = body(700_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let seen = Mutex::new(Vec::new());
        download(&server.url("/file"), &target, &expected(&data), &options(), &|done, total| seen.lock().unwrap().push((done, total))).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert!(!partial_path(&target).exists());
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.first(), Some(&(0, 700_000)));
        assert_eq!(seen.last(), Some(&(700_000, 700_000)));
        assert!(seen.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        // A file that is already there and matches needs no request.
        download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
        assert_eq!(server.seen().len(), 1);
    }

    #[test]
    fn resumes_with_the_exact_range() {
        let data = body(300_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        fs::write(partial_path(&target), &data[..120_000]).unwrap();
        let first = Mutex::new(None);
        download(&server.url("/file"), &target, &expected(&data), &options(), &|done, _| {
            first.lock().unwrap().get_or_insert(done);
        })
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(*first.lock().unwrap(), Some(120_000));
        assert_eq!(server.seen()[0].range.as_deref(), Some("bytes=120000-"));
    }

    fn parallel_options(connections: u64) -> Options {
        Options {
            connections,
            parallel_from: 1,
            ..options()
        }
    }

    fn ranges_seen(server: &Server) -> Vec<Option<String>> {
        let mut ranges: Vec<Option<String>> = server.seen().into_iter().map(|seen| seen.range).collect();
        ranges.sort();
        ranges
    }

    /// One real file over one connection and over the default number, for the time each takes. Run by hand with
    /// `RAINY_QA_DOWNLOAD="<https url> <bytes> <sha256 hex>"`: `cargo test -p rainy-carrier compares -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn compares_one_connection_with_several() {
        let setting = std::env::var("RAINY_QA_DOWNLOAD").expect("RAINY_QA_DOWNLOAD");
        let parts: Vec<&str> = setting.split_whitespace().collect();
        let expected = Expected {
            bytes: parts[1].parse().unwrap(),
            digest: Digest::sha256_hex(parts[2]).unwrap(),
        };
        for connections in [1, CONNECTIONS] {
            let dir = tempfile::tempdir().unwrap();
            let options = Options {
                connections,
                ..Options::new(Arc::new(AtomicBool::new(false)))
            };
            let started = Instant::now();
            download(parts[0], &dir.path().join("file"), &expected, &options, &|_, _| {}).unwrap();
            let seconds = started.elapsed().as_secs_f64();
            println!("{connections} connection(s): {seconds:.1} s, {:.1} MB/s", expected.bytes as f64 / seconds / 1e6);
        }
    }

    #[test]
    fn splits_the_same_file_the_same_way() {
        assert_eq!(pieces(10, 3), [(0, 3), (3, 6), (6, 10)]);
        assert_eq!(pieces(2, 6), [(0, 1), (1, 2)]);
        assert_eq!(pieces(0, 4), [(0, 0)]);
    }

    #[test]
    fn receives_a_large_file_over_several_connections() {
        let data = body(1_000_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let last = Mutex::new(0);
        download(
            &server.url("/file"),
            &target,
            &expected(&data),
            &parallel_options(4),
            &|done, total| {
                assert!(done <= total);
                *last.lock().unwrap() = done;
            },
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(*last.lock().unwrap(), 1_000_000);
        assert_eq!(
            ranges_seen(&server),
            ["bytes=0-249999", "bytes=250000-499999", "bytes=500000-749999", "bytes=750000-999999"].map(|range| Some(range.to_owned()))
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "pieces were left behind");
    }

    #[test]
    fn a_cancelled_parallel_download_resumes_each_piece() {
        let data = body(800_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served).stall_after(50_000, Duration::from_secs(20)));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let options = parallel_options(4);
        let cancel = options.cancel.clone();
        let result = download(&server.url("/file"), &target, &expected(&data), &options, &|done, _| {
            if done >= 200_000 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(result, Err(DownloadError::Cancelled));
        for index in 0..4 {
            assert_eq!(fs::metadata(piece_path(&target, index)).unwrap().len(), 50_000, "piece {index}");
        }
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        download(&server.url("/file"), &target, &expected(&data), &parallel_options(4), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(
            ranges_seen(&server),
            ["bytes=250000-399999", "bytes=450000-599999", "bytes=50000-199999", "bytes=650000-799999"].map(|range| Some(range.to_owned()))
        );
    }

    #[test]
    fn parallel_recovers_from_stale_cached_pieces() {
        let data = body(300_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        // Seed a corrupted cached first piece of valid length so join_pieces sees a digest mismatch.
        fs::write(piece_path(&target, 0), vec![0xEE; 50_000]).unwrap();
        download(&server.url("/file"), &target, &expected(&data), &parallel_options(3), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_server_without_ranges_gets_one_connection() {
        let data = body(300_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        download(&server.url("/file"), &target, &expected(&data), &parallel_options(3), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        let seen = server.seen();
        assert_eq!(seen.last().unwrap().range, None);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_join_that_stopped_part_way_resumes_over_one_connection() {
        let data = body(300_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        // The first two pieces were joined, the third was being appended and is still there.
        fs::write(partial_path(&target), &data[..200_000]).unwrap();
        fs::write(piece_path(&target, 2), &data[200_000..]).unwrap();
        download(&server.url("/file"), &target, &expected(&data), &parallel_options(3), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(ranges_seen(&server), [Some("bytes=200000-".to_owned())]);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_parallel_download_with_the_wrong_digest_fails() {
        let data = body(300_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let wrong = Expected { bytes: 300_000, digest: sha256(b"other") };
        assert_eq!(
            download(&server.url("/file"), &target, &wrong, &parallel_options(3), &|_, _| {}),
            Err(DownloadError::Mismatch)
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn starts_over_when_the_server_ignores_the_range() {
        let data = body(200_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        // Different bytes than the server's: a restart must not keep them.
        fs::write(partial_path(&target), vec![0xEE; 50_000]).unwrap();
        download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert_eq!(server.seen()[0].range.as_deref(), Some("bytes=50000-"));
    }

    #[test]
    fn starts_over_after_another_content_range() {
        let data = body(10_000);
        let served = data.clone();
        // Resumed from the wrong place, or not resumable at all; a request from the start gets the whole file.
        let server = Server::start(move |seen| match seen.range.as_deref() {
            Some("bytes=2000-") => Reply::partial(served[1000..].to_vec(), "bytes 1000-9999/10000"),
            Some(_) => Reply::status(416),
            None => Reply::ok(served.clone()),
        });
        let dir = tempfile::tempdir().unwrap();
        for kept in [2000, 3000] {
            let target = dir.path().join(format!("file-{kept}.bin"));
            fs::write(partial_path(&target), &data[..kept]).unwrap();
            download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
            assert!(!partial_path(&target).exists());
        }
        let ranges: Vec<Option<String>> = server.seen().into_iter().map(|seen| seen.range).collect();
        assert_eq!(ranges, [Some("bytes=2000-".to_owned()), None, Some("bytes=3000-".to_owned()), None]);
        // A server that cannot send the file from the start either fails once more, and nothing is kept.
        let broken = Server::start(|_| Reply::status(416));
        let target = dir.path().join("broken.bin");
        fs::write(partial_path(&target), &data[..2000]).unwrap();
        assert_eq!(download(&broken.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Status(416)));
        assert_eq!(broken.seen().len(), 2);
    }

    #[test]
    fn starts_over_when_a_resumed_file_is_not_the_expected_one() {
        // The partial file of an earlier version of the file, which had the same name.
        let data = body(50_000);
        let served = data.clone();
        let server = Server::start(move |seen| Reply::ranged(seen, &served));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("linux-runtime.tar.xz");
        fs::write(partial_path(&target), vec![0x5A; 20_000]).unwrap();
        download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        let ranges: Vec<Option<String>> = server.seen().into_iter().map(|seen| seen.range).collect();
        assert_eq!(ranges, [Some("bytes=20000-".to_owned()), None]);
    }

    #[test]
    fn follows_redirects() {
        let data = body(5000);
        let served = data.clone();
        let server = Server::start(move |seen| match seen.path.as_str() {
            "/start" => Reply::redirect(302, "/middle/next"),
            "/middle/next" => Reply::redirect(307, "final?x=1"),
            "/middle/final?x=1" => Reply::redirect(301, &format!("http://127.0.0.1:{}/file", seen.port)),
            "/file" => Reply::ok(served.clone()),
            "/away" => Reply::redirect(302, "http://example.com/file"),
            "/loop" => Reply::redirect(302, "/loop"),
            _ => Reply::status(404),
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        download(&server.url("/start"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        let paths: Vec<String> = server.seen().into_iter().map(|seen| seen.path).collect();
        assert_eq!(paths, ["/start", "/middle/next", "/middle/final?x=1", "/file"]);
        // Leaving HTTPS (here: loopback) for plain HTTP elsewhere is refused before any connection.
        let other = dir.path().join("other.bin");
        assert_eq!(download(&server.url("/away"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Insecure("http://example.com/file".into())));
        assert_eq!(download(&server.url("/loop"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Redirects));
        assert_eq!(download(&server.url("/missing"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Status(404)));
    }

    #[test]
    fn stops_when_too_many_bytes_arrive() {
        let data = body(4000);
        let longer = body(9000);
        let server = Server::start(move |_| Reply::ok(longer.clone()));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::TooLarge));
        assert!(!target.exists());
        assert!(fs::metadata(partial_path(&target)).unwrap().len() <= 4000);
    }

    #[test]
    fn cancelling_keeps_the_partial_file() {
        let data = body(400_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()).stall_after(100_000, Duration::from_secs(20)));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let options = options();
        let cancel = options.cancel.clone();
        let started = Instant::now();
        // Cancelled from another thread while the download waits for the stalled server.
        let result = download(&server.url("/file"), &target, &expected(&data), &options, &|done, _| {
            if done >= 100_000 {
                let cancel = cancel.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    cancel.store(true, Ordering::Relaxed);
                });
            }
        });
        assert_eq!(result, Err(DownloadError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(10), "cancelling took {:?}", started.elapsed());
        assert_eq!(fs::read(partial_path(&target)).unwrap(), &data[..100_000]);
        assert!(!target.exists());
    }

    #[test]
    fn a_stalled_download_fails_and_keeps_its_part() {
        let data = body(50_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()).stall_after(20_000, Duration::from_secs(6)));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let options = Options { stall_timeout: Duration::from_secs(1), ..options() };
        assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options, &|_, _| {}), Err(DownloadError::Stalled(1)));
        assert_eq!(fs::read(partial_path(&target)).unwrap(), &data[..20_000]);
    }

    #[test]
    fn a_slow_body_within_the_timeout_arrives() {
        let data = body(40_000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()).paced(8_000, Duration::from_millis(150)));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let options = Options { stall_timeout: Duration::from_secs(2), ..options() };
        download(&server.url("/file"), &target, &expected(&data), &options, &|_, _| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
    }

    #[test]
    fn rejects_a_digest_mismatch() {
        let data = body(30_000);
        let mut wrong = data.clone();
        wrong[29_999] ^= 1;
        let server = Server::start(move |_| Reply::ok(wrong.clone()));
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.bin");
        assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
        assert!(!target.exists());
        assert!(!partial_path(&target).exists());
        // A complete partial file that does not match is replaced, not trusted.
        fs::write(partial_path(&target), vec![1u8; 30_000]).unwrap();
        assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
        assert_eq!(server.seen().last().unwrap().range, None);
        // A partial file larger than the file (another version's) is replaced from the start.
        fs::write(partial_path(&target), vec![1u8; 30_001]).unwrap();
        assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
        assert_eq!(server.seen().last().unwrap().range, None);
    }

    #[test]
    fn verifies_installers_by_sha512() {
        let data = body(3000);
        let served = data.clone();
        let server = Server::start(move |_| Reply::ok(served.clone()));
        let mut sink = HashSink::new(&Digest::Sha512([0; 64]));
        sink.update(&data);
        let digest = sink.finish();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("setup.exe");
        download(&server.url("/setup.exe"), &target, &Expected { bytes: 3000, digest: digest.clone() }, &options(), &|_, _| {}).unwrap();
        assert_eq!(file_digest(&target, &digest, &AtomicBool::new(false)).unwrap(), digest);
    }

    #[test]
    fn fetches_small_documents() {
        let server = Server::start(|seen| match seen.path.as_str() {
            "/latest.yml" => Reply::ok(b"version: 4.0.0\n".to_vec()),
            "/moved" => Reply::redirect(302, "/latest.yml"),
            _ => Reply::status(404),
        });
        assert_eq!(fetch(&server.url("/moved"), 1024, &options()).unwrap(), b"version: 4.0.0\n");
        assert_eq!(fetch(&server.url("/latest.yml"), 4, &options()), Err(DownloadError::TooLarge));
        assert_eq!(fetch(&server.url("/nothing"), 1024, &options()), Err(DownloadError::Status(404)));
        let strict = Options::new(Arc::new(AtomicBool::new(false)));
        assert!(matches!(fetch(&server.url("/latest.yml"), 1024, &strict), Err(DownloadError::Insecure(_))));
    }
}
