//! Multi-connection ranged downloads with piece resuming, digest verification, and stale-cache recovery.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use super::url::Span;
use super::{CHUNK, DownloadError, Expected, HashSink, Options, check_path, get, hash_from, io_error, partial_path, rename};

/// `<target>.partial.<index>`: one piece of a file received over several connections.
pub(super) fn piece_path(target: &Path, index: u64) -> PathBuf {
    let mut name = partial_path(target).into_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

pub(super) fn remove_pieces(target: &Path, count: u64) {
    for index in 0..count.max(1) {
        let _ = fs::remove_file(piece_path(target, index));
    }
}

/// The byte ranges of the pieces, `[start, end)`: `count` nearly equal ones. The same file always gets the same ones,
/// so pieces left by a cancelled download are resumed where they stopped. Uses `u128` arithmetic to avoid overflow.
pub(super) fn pieces(bytes: u64, count: u64) -> Vec<(u64, u64)> {
    let count = count.clamp(1, bytes.max(1));
    let total = u128::from(bytes);
    let divisor = u128::from(count);
    (0..count)
        .map(|index| {
            let i = u128::from(index);
            ((total * i / divisor) as u64, (total * (i + 1) / divisor) as u64)
        })
        .collect()
}

/// Decide the outcome of a batch of parallel workers: any range-rejection error triggers a single-connection fallback
/// before transient errors from sibling workers mask it.
pub(super) fn classify_worker_errors(results: &[Result<(), DownloadError>]) -> Result<bool, DownloadError> {
    if results.iter().any(|result| {
        matches!(
            result,
            Err(DownloadError::InvalidResume | DownloadError::InvalidResponse | DownloadError::Status(416))
        )
    }) {
        return Ok(false);
    }
    if let Some(error) = results
        .iter()
        .find_map(|result| result.as_ref().err().filter(|error| **error != DownloadError::Cancelled))
    {
        return Err(error.clone());
    }
    if results.iter().any(|result| matches!(result, Err(DownloadError::Cancelled))) {
        return Err(DownloadError::Cancelled);
    }
    Ok(true)
}

/// Receive the file over several connections at once, each a range of it into a piece file of its own, whose length
/// is how far it got; then join the pieces into `<target>.partial`, checking the digest on the way. False when the
/// server does not send ranges. If reused cached pieces fail digest verification, they are discarded and retried once.
pub(super) fn parallel(
    url: &str,
    target: &Path,
    expected: &Expected,
    options: &Options,
    progress: &dyn Fn(u64, u64),
) -> Result<bool, DownloadError> {
    let mut retried = false;
    loop {
        let ranges = pieces(expected.bytes, options.connections);
        let paths: Vec<PathBuf> = (0..ranges.len() as u64).map(|index| piece_path(target, index)).collect();
        for path in &paths {
            check_path(path)?;
        }
        let mut kept = 0u64;
        for (path, (start, end)) in paths.iter().zip(&ranges) {
            match fs::symlink_metadata(path) {
                Ok(metadata) if !metadata.is_file() => return Err(DownloadError::InvalidCache),
                Ok(metadata) if metadata.len() > end - start => {
                    fs::remove_file(path).map_err(|error| io_error(path, error))?;
                }
                Ok(metadata) => kept += metadata.len(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(path, error)),
            }
        }
        // The pieces stop together: when the caller cancels, and when one of them fails for good.
        let stop = Arc::new(AtomicBool::new(false));
        let piece_options = Options {
            cancel: stop.clone(),
            ..options.clone()
        };
        let received = AtomicU64::new(kept);
        progress(kept, expected.bytes);
        let results: Vec<Result<(), DownloadError>> = std::thread::scope(|scope| {
            let workers: Vec<_> = paths
                .iter()
                .zip(&ranges)
                .map(|(path, &(start, end))| {
                    let (stop, piece_options, received) = (&stop, &piece_options, &received);
                    scope.spawn(move || {
                        let result = receive_piece(url, path, start, end, expected.bytes, piece_options, received);
                        if !matches!(result, Ok(()) | Err(DownloadError::Cancelled)) {
                            stop.store(true, Ordering::Relaxed);
                        }
                        result
                    })
                })
                .collect();
            while !workers.iter().all(|worker| worker.is_finished()) {
                if options.cancel.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                }
                progress(received.load(Ordering::Relaxed), expected.bytes);
                std::thread::sleep(Duration::from_millis(100));
            }
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap_or(Err(DownloadError::Interrupted)))
                .collect()
        });
        options.cancelled()?;
        if !classify_worker_errors(&results)? {
            return Ok(false);
        }
        match join_pieces(target, &paths, expected, options) {
            Ok(()) => {
                progress(expected.bytes, expected.bytes);
                return Ok(true);
            }
            Err(DownloadError::Mismatch) if kept > 0 && !retried => {
                log::info!("{} had stale cached pieces; clearing pieces and downloading again", target.display());
                remove_pieces(target, options.connections);
                let _ = fs::remove_file(partial_path(target));
                retried = true;
            }
            Err(error) => {
                if error == DownloadError::Mismatch {
                    remove_pieces(target, options.connections);
                }
                return Err(error);
            }
        }
    }
}

/// Receive bytes `[start, end)` of a file of `total` bytes into `path`, from where it stopped; a connection that drops
/// is tried again twice.
fn receive_piece(
    url: &str,
    path: &Path,
    start: u64,
    end: u64,
    total: u64,
    options: &Options,
    received: &AtomicU64,
) -> Result<(), DownloadError> {
    let failed = |error| io_error(path, error);
    let mut attempt = 0;
    loop {
        let done = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(failed(error)),
        };
        if done >= end - start {
            return Ok(());
        }
        let from = start + done;
        let result = (|| -> Result<(), DownloadError> {
            let mut response = get(
                url,
                Span {
                    start: from,
                    end: Some(end - 1),
                },
                options,
            )?;
            match response.status {
                206 if response.content_range.as_deref() == Some(format!("bytes {from}-{}/{total}", end - 1).as_str()) => {}
                206 | 200 => return Err(DownloadError::InvalidResume),
                201..=299 => return Err(DownloadError::InvalidResponse),
                status => return Err(DownloadError::Status(status)),
            }
            let mut file = fs::OpenOptions::new().create(true).append(true).open(path).map_err(failed)?;
            let (mut at, mut buffer) = (from, vec![0u8; CHUNK]);
            loop {
                options.cancelled()?;
                let read = response.read(&mut buffer, options)?;
                if read == 0 {
                    break;
                }
                if at.saturating_add(read as u64) > end {
                    return Err(DownloadError::TooLarge);
                }
                file.write_all(&buffer[..read]).map_err(failed)?;
                at += read as u64;
                received.fetch_add(read as u64, Ordering::Relaxed);
            }
            file.sync_all().map_err(failed)?;
            if at < end { Err(DownloadError::Interrupted) } else { Ok(()) }
        })();
        match result {
            Err(DownloadError::Network(_) | DownloadError::Stalled(_) | DownloadError::Interrupted)
                if attempt < 2 && !options.cancel.load(Ordering::Relaxed) =>
            {
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// Join the pieces into `<target>.partial` in order, hashing them on the way. The joined file grows only by whole,
/// correct bytes, so a join that stops part-way leaves a `.partial` file a later attempt resumes over one connection.
fn join_pieces(target: &Path, paths: &[PathBuf], expected: &Expected, options: &Options) -> Result<(), DownloadError> {
    let partial = partial_path(target);
    let failed = |path: &Path, error| io_error(path, error);
    let mut sink = HashSink::new(&expected.digest);
    {
        let mut first = File::open(&paths[0]).map_err(|error| failed(&paths[0], error))?;
        hash_from(&mut first, u64::MAX, &mut sink, &options.cancel).map_err(|error| {
            if options.cancel.load(Ordering::Relaxed) {
                DownloadError::Cancelled
            } else {
                failed(&paths[0], error)
            }
        })?;
    }
    rename(&paths[0], &partial).map_err(|error| failed(&partial, error))?;
    let mut joined = fs::OpenOptions::new().append(true).open(&partial).map_err(|error| failed(&partial, error))?;
    let mut buffer = vec![0u8; CHUNK];
    for path in &paths[1..] {
        let mut piece = File::open(path).map_err(|error| failed(path, error))?;
        loop {
            options.cancelled()?;
            let read = match io::Read::read(&mut piece, &mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(failed(path, error)),
            };
            joined.write_all(&buffer[..read]).map_err(|error| failed(&partial, error))?;
            sink.update(&buffer[..read]);
        }
        drop(piece);
        fs::remove_file(path).map_err(|error| failed(path, error))?;
    }
    joined.sync_all().map_err(|error| failed(&partial, error))?;
    drop(joined);
    if sink.finish() != expected.digest {
        let _ = fs::remove_file(&partial);
        return Err(DownloadError::Mismatch);
    }
    Ok(())
}
