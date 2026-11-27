//! Asynchronous WinHTTP transport with pooled sessions and cancellation-safe handle teardown.
//!
//! Each request runs on an asynchronous WinHTTP session (`WINHTTP_FLAG_ASYNC`) with a status callback that signals a
//! `Condvar`. The calling thread is the sole owner of the request handle and performs all `WinHttp*` calls and the
//! final `WinHttpCloseHandle` serially, waiting for `WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING` before returning or
//! releasing any read buffer. This eliminates both cross-thread handle close races/UAF and per-request watcher threads.

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::Networking::WinHttp::*;

    use super::super::url::{Address, Span, is_loopback};
    use super::super::{DownloadError, Options};
    use crate::text::wide;

    const CANCEL_POLL: Duration = Duration::from_millis(50);

    /// A WinHTTP session or connection handle, closed when dropped.
    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a valid handle returned by WinHTTP, closed once.
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    /// Either a process-wide pooled session (never closed) or a one-off fallback session (closed on drop).
    enum SessionRef {
        Shared(*mut c_void),
        Owned(Handle),
    }

    impl SessionRef {
        fn as_ptr(&self) -> *mut c_void {
            match self {
                Self::Shared(ptr) => *ptr,
                Self::Owned(handle) => handle.0,
            }
        }
    }

    struct SharedSession(usize);

    fn open_async_session(access: WINHTTP_ACCESS_TYPE, proxy: Option<&str>) -> *mut c_void {
        let agent = wide("Rainy");
        let (proxy, bypass) = (proxy.map(wide), wide("<local>"));
        let (proxy_ptr, bypass_ptr) = match &proxy {
            Some(proxy) => (proxy.as_ptr(), bypass.as_ptr()),
            None => (std::ptr::null(), std::ptr::null()),
        };
        // SAFETY: NUL-terminated UTF-16 strings outlive the call; `WINHTTP_FLAG_ASYNC` enables async callbacks.
        unsafe { WinHttpOpen(agent.as_ptr(), access, proxy_ptr, bypass_ptr, WINHTTP_FLAG_ASYNC) }
    }

    /// Acquire a pooled asynchronous session for `address` to preserve connection pooling and TLS session reuse.
    fn session(address: &Address) -> Result<SessionRef, DownloadError> {
        static NO_PROXY: OnceLock<Result<SharedSession, u32>> = OnceLock::new();
        static AUTO_PROXY: OnceLock<Option<SharedSession>> = OnceLock::new();

        if is_loopback(&address.host) {
            let shared = NO_PROXY.get_or_init(|| {
                let handle = open_async_session(WINHTTP_ACCESS_TYPE_NO_PROXY, None);
                if handle.is_null() {
                    // SAFETY: reads the calling thread's last error.
                    Err(unsafe { GetLastError() })
                } else {
                    Ok(SharedSession(handle as usize))
                }
            });
            return match shared {
                Ok(session) => Ok(SessionRef::Shared(session.0 as *mut c_void)),
                Err(code) => Err(DownloadError::Network(describe(*code))),
            };
        }

        if let Some(session) = AUTO_PROXY.get_or_init(|| {
            let handle = open_async_session(WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None);
            (!handle.is_null()).then_some(SharedSession(handle as usize))
        }) {
            return Ok(SessionRef::Shared(session.0 as *mut c_void));
        }

        // Fallback when `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` is unavailable.
        let handle = match crate::proxy::system_proxy(&address.url()) {
            Some(proxy) => {
                let trimmed = proxy
                    .strip_prefix("http://")
                    .or_else(|| proxy.strip_prefix("https://"))
                    .unwrap_or(&proxy);
                open_async_session(WINHTTP_ACCESS_TYPE_NAMED_PROXY, Some(trimmed))
            }
            None => open_async_session(WINHTTP_ACCESS_TYPE_NO_PROXY, None),
        };
        if handle.is_null() {
            // SAFETY: reads the calling thread's last error.
            return Err(DownloadError::Network(describe(unsafe { GetLastError() })));
        }
        Ok(SessionRef::Owned(Handle(handle)))
    }

    #[derive(Debug, Clone, Copy)]
    enum Event {
        SendComplete,
        HeadersAvailable,
        DataAvailable(u32),
        ReadComplete(usize),
        Error(u32),
    }

    #[derive(Default)]
    struct CallbackState {
        event: Option<Event>,
        closed: bool,
    }

    struct AsyncState {
        state: Mutex<CallbackState>,
        ready: Condvar,
    }

    impl AsyncState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(CallbackState::default()),
                ready: Condvar::new(),
            })
        }
    }

    unsafe extern "system" fn status_callback(
        _handle: *mut c_void,
        context: usize,
        status: u32,
        info: *mut c_void,
        info_len: u32,
    ) {
        if context == 0 {
            return;
        }
        let ptr = context as *const AsyncState;
        if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
            // SAFETY: `context` carries the `Arc` reference handed to WinHTTP when installing this callback;
            // `HANDLE_CLOSING` is guaranteed to be the final callback for this handle.
            let state = unsafe { Arc::from_raw(ptr) };
            let mut guard = state.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.closed = true;
            state.ready.notify_all();
            return;
        }
        let event = match status {
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => Some(Event::SendComplete),
            WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => Some(Event::HeadersAvailable),
            WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE => {
                // SAFETY: WinHTTP passes a pointer to a `u32` count for `DATA_AVAILABLE`.
                let available = if !info.is_null() && info_len as usize >= size_of::<u32>() {
                    unsafe { *(info as *const u32) }
                } else {
                    0
                };
                Some(Event::DataAvailable(available))
            }
            WINHTTP_CALLBACK_STATUS_READ_COMPLETE => Some(Event::ReadComplete(info_len as usize)),
            WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
                // SAFETY: WinHTTP passes a pointer to `WINHTTP_ASYNC_RESULT` for `REQUEST_ERROR`.
                let code = if !info.is_null() && info_len as usize >= size_of::<WINHTTP_ASYNC_RESULT>() {
                    unsafe { (*(info as *const WINHTTP_ASYNC_RESULT)).dwError }
                } else {
                    ERROR_WINHTTP_INTERNAL_ERROR
                };
                Some(Event::Error(code))
            }
            _ => None,
        };
        if let Some(event) = event {
            // SAFETY: `context` stays valid until `HANDLE_CLOSING` reclaims the `Arc`.
            let state = unsafe { &*ptr };
            let mut guard = state.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.event = Some(event);
            state.ready.notify_one();
        }
    }

    /// Exclusive owner of one async WinHTTP request handle.
    struct AsyncRequest {
        handle: Option<*mut c_void>,
        state: Arc<AsyncState>,
        context: usize,
        callback_registered: bool,
        _connection: Handle,
        _session: SessionRef,
    }

    impl AsyncRequest {
        fn open(address: &Address, options: &Options) -> Result<Self, DownloadError> {
            let session = session(address)?;
            let host = wide(address.host.trim_start_matches('[').trim_end_matches(']'));
            // SAFETY: valid session handle and NUL-terminated host string.
            let connection = unsafe { WinHttpConnect(session.as_ptr(), host.as_ptr(), address.port, 0) };
            if connection.is_null() {
                return Err(last_error(options));
            }
            let connection = Handle(connection);
            let (verb, path) = (wide("GET"), wide(&address.path));
            let flags = if address.secure { WINHTTP_FLAG_SECURE } else { 0 };
            // SAFETY: valid connection handle and NUL-terminated verb/path strings.
            let request = unsafe {
                WinHttpOpenRequest(
                    connection.0,
                    verb.as_ptr(),
                    path.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    flags,
                )
            };
            if request.is_null() {
                return Err(last_error(options));
            }

            let state = AsyncState::new();
            let context = Arc::into_raw(Arc::clone(&state)) as usize;
            // SAFETY: binds `context` to the request handle so `HANDLE_CLOSING` always sees it.
            let set_ctx = unsafe {
                WinHttpSetOption(
                    request,
                    WINHTTP_OPTION_CONTEXT_VALUE,
                    (&raw const context).cast(),
                    size_of::<usize>() as u32,
                )
            };
            if set_ctx == 0 {
                let error = last_error(options);
                // SAFETY: callback was never installed; reclaim the raw `Arc` and close the handle.
                unsafe {
                    WinHttpCloseHandle(request);
                    drop(Arc::from_raw(context as *const AsyncState));
                }
                return Err(error);
            }

            // SAFETY: `status_callback` matches `WINHTTP_STATUS_CALLBACK`; `request` is a valid request handle.
            let prev = unsafe {
                WinHttpSetStatusCallback(
                    request,
                    Some(status_callback),
                    WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS,
                    0,
                )
            };
            // `WINHTTP_INVALID_STATUS_CALLBACK` is `(WINHTTP_STATUS_CALLBACK)(-1)`.
            if prev.is_some_and(|f| f as usize == usize::MAX) {
                let error = last_error(options);
                let zero = 0usize;
                // SAFETY: clear context before closing so nothing dereferences `context`, then reclaim the `Arc`.
                unsafe {
                    WinHttpSetOption(
                        request,
                        WINHTTP_OPTION_CONTEXT_VALUE,
                        (&raw const zero).cast(),
                        size_of::<usize>() as u32,
                    );
                    WinHttpCloseHandle(request);
                    drop(Arc::from_raw(context as *const AsyncState));
                }
                return Err(error);
            }

            Ok(Self {
                handle: Some(request),
                state,
                context,
                callback_registered: true,
                _connection: connection,
                _session: session,
            })
        }

        fn clear_event(&self) {
            let mut guard = self.state.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.event = None;
        }

        /// Close the request handle and wait until WinHTTP delivers `HANDLE_CLOSING`, guaranteeing that no async
        /// callback or read operation can still touch caller buffers or the handle.
        fn close_and_wait(&mut self) {
            let Some(handle) = self.handle.take() else { return };
            // SAFETY: `handle` is exclusively owned and closed once; no lock is held during `WinHttpCloseHandle`
            // because WinHTTP may invoke `HANDLE_CLOSING` synchronously on this thread.
            unsafe { WinHttpCloseHandle(handle) };
            if self.callback_registered {
                let mut guard = self.state.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                while !guard.closed {
                    guard = self.state.ready.wait(guard).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }

        fn wait_event(&mut self, options: &Options) -> Result<Event, DownloadError> {
            let deadline = Instant::now() + options.stall_timeout;
            let mut guard = self.state.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(event) = guard.event.take() {
                    return Ok(event);
                }
                if options.cancel.load(Ordering::Relaxed) {
                    drop(guard);
                    self.close_and_wait();
                    return Err(DownloadError::Cancelled);
                }
                let now = Instant::now();
                if now >= deadline {
                    drop(guard);
                    self.close_and_wait();
                    return Err(DownloadError::Stalled(options.stall_timeout.as_secs()));
                }
                let slice = (deadline - now).min(CANCEL_POLL);
                let (next, _) = self
                    .state
                    .ready
                    .wait_timeout(guard, slice)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard = next;
            }
        }
    }

    impl Drop for AsyncRequest {
        fn drop(&mut self) {
            self.close_and_wait();
        }
    }

    /// The answer to one request: its status and the headers a download needs, with the body still to read.
    pub(in super::super) struct Response {
        pub status: u16,
        pub location: Option<String>,
        pub content_range: Option<String>,
        request: AsyncRequest,
    }

    impl Response {
        /// The next part of the body, as soon as some of it arrived; 0 at its end.
        pub fn read(&mut self, buffer: &mut [u8], options: &Options) -> Result<usize, DownloadError> {
            options.cancelled()?;
            let handle = self.request.handle.ok_or(DownloadError::Cancelled)?;
            self.request.clear_event();
            // SAFETY: valid async request handle; `null_mut()` is required in async mode.
            if unsafe { WinHttpQueryDataAvailable(handle, std::ptr::null_mut()) } == 0 {
                return Err(last_error(options));
            }
            let available = match self.request.wait_event(options)? {
                Event::DataAvailable(0) => return Ok(0),
                Event::DataAvailable(bytes) => bytes,
                Event::Error(code) => return Err(error_from_code(code, options)),
                _ => return Err(DownloadError::InvalidResponse),
            };
            let handle = self.request.handle.ok_or(DownloadError::Cancelled)?;
            let wanted = buffer.len().min(available as usize).min(u32::MAX as usize) as u32;
            self.request.clear_event();
            // SAFETY: `buffer` is writable for at least `wanted` bytes and remains valid until either `ReadComplete`
            // arrives or `wait_event` closes the handle and waits for `HANDLE_CLOSING`.
            if unsafe { WinHttpReadData(handle, buffer.as_mut_ptr().cast(), wanted, std::ptr::null_mut()) } == 0 {
                return Err(last_error(options));
            }
            match self.request.wait_event(options)? {
                Event::ReadComplete(read) => Ok(read),
                Event::Error(code) => Err(error_from_code(code, options)),
                _ => Err(DownloadError::InvalidResponse),
            }
        }
    }

    fn error_from_code(code: u32, options: &Options) -> DownloadError {
        if options.cancel.load(Ordering::Relaxed) || code == ERROR_WINHTTP_OPERATION_CANCELLED {
            return DownloadError::Cancelled;
        }
        if code == ERROR_WINHTTP_TIMEOUT {
            return DownloadError::Stalled(options.stall_timeout.as_secs());
        }
        DownloadError::Network(describe(code))
    }

    /// The error of the WinHTTP call that just failed.
    fn last_error(options: &Options) -> DownloadError {
        // SAFETY: reads the calling thread's last error.
        error_from_code(unsafe { GetLastError() }, options)
    }

    /// WinHTTP's own description of an error, in the system's language, with its code.
    fn describe(code: u32) -> String {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            FORMAT_MESSAGE_FROM_HMODULE, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
        };
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

        let mut buffer = [0u16; 512];
        let module = wide("winhttp.dll");
        // SAFETY: `buffer` is writable for its length; no inserts are read.
        let length = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_HMODULE | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                GetModuleHandleW(module.as_ptr()) as *const c_void,
                code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null(),
            )
        };
        let text = String::from_utf16_lossy(&buffer[..length as usize])
            .trim()
            .trim_end_matches(['.', '。'])
            .to_owned();
        if text.is_empty() { format!("WinHTTP {code}") } else { format!("{text}, WinHTTP {code}") }
    }

    fn header(handle: *mut c_void, info: u32) -> Option<String> {
        let mut size = 0u32;
        // SAFETY: asks for the size only; the second call writes at most `size` bytes into `buffer`.
        unsafe {
            if WinHttpQueryHeaders(handle, info, std::ptr::null(), std::ptr::null_mut(), &mut size, std::ptr::null_mut()) != 0
                || GetLastError() != ERROR_INSUFFICIENT_BUFFER
            {
                return None;
            }
            let mut buffer = vec![0u16; size as usize / 2 + 1];
            if WinHttpQueryHeaders(handle, info, std::ptr::null(), buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut()) == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer[..size as usize / 2]))
        }
    }

    /// One GET without following redirects; no cookies are kept or sent.
    pub(in super::super) fn request(address: &Address, span: Span, options: &Options) -> Result<Response, DownloadError> {
        options.cancelled()?;
        let mut req = AsyncRequest::open(address, options)?;
        let handle = req.handle.ok_or(DownloadError::Cancelled)?;
        let features: u32 = WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS;
        let timeout = options.stall_timeout.as_millis().min(i32::MAX as u128) as i32;
        let range: Vec<u16> = span
            .header()
            .map(|range| format!("Range: {range}\r\n").encode_utf16().collect())
            .unwrap_or_default();
        req.clear_event();
        // SAFETY: valid async request handle; `features` and `range` remain live until `SendComplete` or close.
        let sent = unsafe {
            WinHttpSetOption(handle, WINHTTP_OPTION_DISABLE_FEATURE, (&raw const features).cast(), 4) != 0
                && WinHttpSetTimeouts(handle, timeout, timeout, timeout, timeout) != 0
                && WinHttpSendRequest(
                    handle,
                    if range.is_empty() { std::ptr::null() } else { range.as_ptr() },
                    range.len() as u32,
                    std::ptr::null(),
                    0,
                    0,
                    req.context,
                ) != 0
        };
        if !sent {
            return Err(last_error(options));
        }
        match req.wait_event(options)? {
            Event::SendComplete => {}
            Event::Error(code) => return Err(error_from_code(code, options)),
            _ => return Err(DownloadError::InvalidResponse),
        }
        let handle = req.handle.ok_or(DownloadError::Cancelled)?;
        req.clear_event();
        // SAFETY: request was sent; `WinHttpReceiveResponse` completes asynchronously with `HeadersAvailable`.
        if unsafe { WinHttpReceiveResponse(handle, std::ptr::null_mut()) } == 0 {
            return Err(last_error(options));
        }
        match req.wait_event(options)? {
            Event::HeadersAvailable => {}
            Event::Error(code) => return Err(error_from_code(code, options)),
            _ => return Err(DownloadError::InvalidResponse),
        }
        let handle = req.handle.ok_or(DownloadError::Cancelled)?;
        let mut status = 0u32;
        let mut size = 4u32;
        // SAFETY: `status` is a writable u32, as `WINHTTP_QUERY_FLAG_NUMBER` asks.
        let queried = unsafe {
            WinHttpQueryHeaders(
                handle,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&raw mut status).cast(),
                &mut size,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(DownloadError::InvalidResponse);
        }
        let status = u16::try_from(status).map_err(|_| DownloadError::InvalidResponse)?;
        let location = header(handle, WINHTTP_QUERY_LOCATION);
        let content_range = header(handle, WINHTTP_QUERY_CONTENT_RANGE);
        Ok(Response {
            status,
            location,
            content_range,
            request: req,
        })
    }
}

#[cfg(not(windows))]
mod platform {
    use super::super::url::{Address, Span};
    use super::super::{DownloadError, Options};

    pub(in super::super) struct Response {
        pub status: u16,
        pub location: Option<String>,
        pub content_range: Option<String>,
    }

    impl Response {
        pub fn read(&mut self, _buffer: &mut [u8], _options: &Options) -> Result<usize, DownloadError> {
            Err(DownloadError::Network("downloads need Windows".into()))
        }
    }

    pub(in super::super) fn request(_address: &Address, _span: Span, _options: &Options) -> Result<Response, DownloadError> {
        Err(DownloadError::Network("downloads need Windows".into()))
    }
}

pub(super) use self::platform::{Response, request};
