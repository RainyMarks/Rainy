//! A WebView2 control (the Edge engine Windows ships) as a child of the GPUI window.
//!
//! Creation is asynchronous: WebView2 answers through window messages that GPUI's own message loop dispatches, so
//! nothing here runs a nested message loop while GPUI is in the middle of an update.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CallDevToolsProtocolMethodCompletedHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler, DevToolsProtocolEventReceivedEventHandler,
    ExecuteScriptCompletedHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, WebMessageReceivedEventHandler, WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR};

type Waiter = Box<dyn FnOnce(Result<ICoreWebView2Environment, String>)>;
type CreatedCallback = Box<dyn FnOnce(Result<WebView, String>)>;
type UrlPredicate = Box<dyn FnMut(&str) -> bool>;

enum Environment {
    Creating(Vec<Waiter>),
    Ready(ICoreWebView2Environment),
}

thread_local! {
    /// One environment (browser process) per profile folder.
    static ENVIRONMENTS: RefCell<HashMap<PathBuf, Environment>> = RefCell::default();
}

/// Drop a cached WebView2 environment for a temporary profile folder (such as the agent preview) so its COM handle
/// and browser process can exit and release file locks on the folder.
pub fn release_environment(folder: &Path) {
    ENVIRONMENTS.with(|map| {
        map.borrow_mut().remove(folder);
    });
}

fn describe(error: &windows::core::Error) -> String {
    // HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND): no WebView2 runtime is installed.
    if error.code().0 as u32 == 0x8007_0002 {
        return crate::i18n::text(crate::i18n::Locale::current(), "native.webview2Missing").to_owned();
    }
    format!("WebView2: {} ({:#010x})", error.message(), error.code().0)
}

/// The environment of a profile folder, created on first use. Failed attempts are not permanently cached so transient
/// errors or a later WebView2 Runtime install can recover.
fn environment(folder: &Path, done: Waiter) {
    let mut done = Some(done);
    let ready = ENVIRONMENTS.with(|map| {
        let mut map = map.borrow_mut();
        match map.get_mut(folder) {
            Some(Environment::Ready(environment)) => Some(Ok(environment.clone())),
            Some(Environment::Creating(waiters)) => {
                waiters.extend(done.take());
                None
            }
            None => {
                map.insert(folder.to_owned(), Environment::Creating(done.take().into_iter().collect()));
                None
            }
        }
    });
    if let (Some(result), Some(done)) = (ready, done.take()) {
        done(result);
        return;
    }
    let starting = ENVIRONMENTS.with(|map| matches!(map.borrow().get(folder), Some(Environment::Creating(waiters)) if waiters.len() == 1));
    if !starting {
        return;
    }
    let key = folder.to_owned();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, environment| {
        let outcome = result.map_err(|error| describe(&error)).and_then(|()| environment.ok_or_else(|| "WebView2 returned no environment.".to_owned()));
        settle(&key, outcome);
        Ok(())
    }));
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: plain COM calls on the UI thread with live arguments.
    let started = unsafe {
        // No mini menu over selected text, no PDF menu of Edge's own and no SmartScreen lookups of local pages.
        options.set_additional_browser_arguments("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_owned());
        // Context menus, media controls and the pages' `navigator.language` follow the interface language. An
        // environment keeps the language it was created with, so a change in Settings applies after a restart.
        options.set_language(rainy_carrier::locale::localized("zh-CN", "en-US"));
        CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &HSTRING::from(folder), &ICoreWebView2EnvironmentOptions::from(options), &handler)
    };
    if let Err(error) = started {
        settle(folder, Err(describe(&error)));
    }
}

/// Whether this process was started for page QA with a DevTools Protocol port
/// (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=…`), which keeps DevTools enabled in release builds.
fn remote_debugging() -> bool {
    std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_ok_and(|arguments| arguments.contains("--remote-debugging-port"))
}

/// The version of the WebView2 runtime new controls would use, such as `141.0.3537.71`; otherwise why there is none.
pub fn runtime_version() -> Result<String, String> {
    let mut version = PWSTR::null();
    // SAFETY: no browser folder (the installed runtime); WebView2 allocates the string, which `take_pwstr` frees.
    match unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) } {
        Ok(()) if !version.is_null() => Ok(take_pwstr(version)),
        Err(error) if error.code().0 as u32 != 0x8007_0002 => Err(format!("{} ({:#010x})", error.message(), error.code().0)),
        _ => Err("not installed".to_owned()),
    }
}

fn settle(folder: &Path, outcome: Result<ICoreWebView2Environment, String>) {
    let waiters = ENVIRONMENTS.with(|map| {
        let mut map = map.borrow_mut();
        let previous = match &outcome {
            Ok(environment) => map.insert(folder.to_owned(), Environment::Ready(environment.clone())),
            Err(_) => map.remove(folder),
        };
        match previous {
            Some(Environment::Creating(waiters)) => waiters,
            _ => Vec::new(),
        }
    });
    for waiter in waiters {
        waiter(outcome.clone());
    }
}

/// A key combination pressed while the page had the keyboard: virtual key and modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Whether `url` belongs to exact `origin` (scheme + host + port), preventing prefix spoofing
/// such as `http://127.0.0.1:80` matching `http://127.0.0.1:8080/` or `http://127.0.0.1:80@evil.com`.
pub fn matches_origin(url: &str, origin: &str) -> bool {
    if origin.is_empty() {
        return false;
    }
    let Some(rest) = url.strip_prefix(origin) else {
        return false;
    };
    rest.is_empty() || matches!(rest.as_bytes()[0], b'/' | b'?' | b'#')
}

/// How a WebView is set up.
pub struct Options {
    /// Profile folder: cookies, cache and storage.
    pub folder: PathBuf,
    /// Keep nothing on disk (the agent's preview).
    pub private: bool,
    /// Shown before the page paints.
    pub background: (u8, u8, u8),
    /// Whether JavaScript execution is allowed in the control (false for untrusted HTML PDF export).
    pub scripts_enabled: bool,
    /// Exact origin allowed to receive `window.ipc` and deliver WebMessages; `None` disables IPC injection.
    pub ipc_origin: Option<String>,
    /// Runs in every page before its own scripts (only when `scripts_enabled` is true).
    pub script: Option<String>,
    /// Strings the page posts with `window.ipc.postMessage`.
    pub on_message: Option<Box<dyn FnMut(String)>>,
    /// Whether the page may go to an address; refused navigations are cancelled.
    pub allow: Option<UrlPredicate>,
    /// Whether subresource/fetch/XHR requests may reach an address; refused requests get a synthetic 403.
    pub allow_resource: Option<UrlPredicate>,
    /// The page asked for a new window; it never opens one, the address goes here.
    pub on_new_window: Option<Box<dyn FnMut(String)>>,
    /// Combinations the page must not keep (the workbench's shortcuts); `true` takes the key from the page.
    pub on_shortcut: Option<Box<dyn FnMut(Shortcut) -> bool>>,
}

/// One WebView2 control.
#[derive(Clone)]
pub struct WebView {
    controller: ICoreWebView2Controller,
    view: ICoreWebView2,
}

impl WebView {
    /// Create a control in `parent`; `done` runs on the UI thread once it exists or failed.
    pub fn create(parent: HWND, options: Options, done: impl FnOnce(Result<WebView, String>) + 'static) {
        let done: Rc<RefCell<Option<CreatedCallback>>> = Rc::new(RefCell::new(Some(Box::new(done))));
        let finish = move |result: Result<WebView, String>| {
            if let Some(done) = done.borrow_mut().take() {
                done(result);
            }
        };
        let private = options.private;
        let folder = options.folder.clone();
        let options = Rc::new(RefCell::new(Some(options)));
        environment(
            &folder,
            Box::new(move |environment| {
                let environment = match environment {
                    Ok(environment) => environment,
                    Err(error) => return finish(Err(error)),
                };
                let made = finish.clone();
                let env_for_setup = environment.clone();
                let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
                    let options = options.borrow_mut().take();
                    let result = result
                        .map_err(|error| describe(&error))
                        .and_then(|()| controller.ok_or_else(|| "WebView2 returned no control.".to_owned()))
                        .and_then(|controller| {
                            let options = options.ok_or_else(|| "WebView2 controller options already consumed.".to_owned())?;
                            WebView::setup(controller, &env_for_setup, options).map_err(|error| describe(&error))
                        });
                    made(result);
                    Ok(())
                }));
                // SAFETY: COM calls on the UI thread; `parent` is the live GPUI window.
                let started = unsafe {
                    match environment.cast::<ICoreWebView2Environment10>() {
                        Ok(environment) => environment.CreateCoreWebView2ControllerOptions().and_then(|settings| {
                            settings.SetIsInPrivateModeEnabled(private)?;
                            environment.CreateCoreWebView2ControllerWithOptions(parent, &settings, &handler)
                        }),
                        Err(_) => environment.CreateCoreWebView2Controller(parent, &handler),
                    }
                };
                if let Err(error) = started {
                    finish(Err(describe(&error)));
                }
            }),
        );
    }

    fn setup(controller: ICoreWebView2Controller, environment: &ICoreWebView2Environment, mut options: Options) -> windows::core::Result<WebView> {
        // SAFETY: COM calls on the UI thread on a control that was just created.
        unsafe {
            let view = controller.CoreWebView2()?;
            if let Ok(controller) = controller.cast::<ICoreWebView2Controller2>() {
                let (r, g, b) = options.background;
                controller.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: r, G: g, B: b })?;
            }
            let settings = view.Settings()?;
            settings.SetIsStatusBarEnabled(false)?;
            settings.SetIsZoomControlEnabled(false)?;
            settings.SetIsScriptEnabled(options.scripts_enabled)?;
            settings.SetIsWebMessageEnabled(options.scripts_enabled && options.ipc_origin.is_some())?;
            settings.SetAreDevToolsEnabled(options.scripts_enabled && (cfg!(debug_assertions) || remote_debugging()))?;
            // The browser's own keys make no sense in an embedded page: F5 / Ctrl+R would reload it and lose the rich
            // editor's undo history, Ctrl+P, Ctrl+U, Ctrl+F, F12 and Alt+Left open browser features. Editing keys
            // (copy, paste, undo, select all) are not browser accelerators and keep working, and the keys reach the
            // page, which handles its own find and zoom. DevTools stay reachable from the context menu and the
            // debugging port where they are enabled above.
            if let Ok(settings) = settings.cast::<ICoreWebView2Settings3>() {
                settings.SetAreBrowserAcceleratorKeysEnabled(false)?;
            }
            let mut token = 0i64;
            let ipc_origin = options.ipc_origin.clone();
            if let Some(mut on_message) = options.on_message.take() {
                let expected_origin = ipc_origin.clone();
                let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        if let Some(expected) = expected_origin.as_deref() {
                            let mut source = PWSTR::null();
                            if args.Source(&mut source).is_err() || !matches_origin(&take_pwstr(source), expected) {
                                return Ok(());
                            }
                        } else {
                            return Ok(());
                        }
                        let mut text = PWSTR::null();
                        if args.TryGetWebMessageAsString(&mut text).is_ok() {
                            on_message(take_pwstr(text));
                        }
                    }
                    Ok(())
                }));
                view.add_WebMessageReceived(&handler, &mut token)?;
            }
            if let Some(mut allow) = options.allow.take() {
                let handler = NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        if !allow(&take_pwstr(uri)) {
                            args.SetCancel(true)?;
                        }
                    }
                    Ok(())
                }));
                view.add_NavigationStarting(&handler, &mut token)?;
            }
            if let Some(mut allow_resource) = options.allow_resource.take() {
                view.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
                let env = environment.clone();
                let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args
                        && let Ok(request) = args.Request()
                    {
                        let mut uri = PWSTR::null();
                        if request.Uri(&mut uri).is_ok() && !allow_resource(&take_pwstr(uri))
                            && let Ok(response) = env.CreateWebResourceResponse(None::<&IStream>, 403, &HSTRING::from("Forbidden"), &HSTRING::from(""))
                        {
                            let _ = args.SetResponse(&response);
                        }
                    }
                    Ok(())
                }));
                view.add_WebResourceRequested(&handler, &mut token)?;
            }
            let mut on_new_window = options.on_new_window.take();
            let handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    if let Some(on_new_window) = on_new_window.as_mut() {
                        on_new_window(uri);
                    }
                }
                Ok(())
            }));
            view.add_NewWindowRequested(&handler, &mut token)?;
            // Camera, microphone, location and notifications are refused rather than asked.
            let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                }
                Ok(())
            }));
            view.add_PermissionRequested(&handler, &mut token)?;
            if let Some(mut on_shortcut) = options.on_shortcut.take() {
                let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                        return Ok(());
                    }
                    let mut key = 0u32;
                    args.VirtualKey(&mut key)?;
                    let down = |key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| GetKeyState(i32::from(key.0)) < 0;
                    let shortcut = Shortcut { key, ctrl: down(VK_CONTROL), shift: down(VK_SHIFT), alt: down(VK_MENU) };
                    if on_shortcut(shortcut) {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                }));
                controller.add_AcceleratorKeyPressed(&handler, &mut token)?;
            }
            if options.scripts_enabled {
                let mut script = String::new();
                if let Some(origin) = ipc_origin.as_deref() {
                    let origin_json = serde_json::to_string(origin).unwrap_or_else(|_| "\"\"".to_owned());
                    script.push_str(&format!(
                        "if (window.location && window.location.origin === {origin_json}) {{ window.ipc = {{ postMessage: (message) => window.chrome.webview.postMessage(String(message)) }}; }}"
                    ));
                }
                if let Some(extra) = options.script.take() {
                    if !script.is_empty() {
                        script.push('\n');
                    }
                    script.push_str(&extra);
                }
                if !script.is_empty() {
                    view.AddScriptToExecuteOnDocumentCreated(
                        &HSTRING::from(script),
                        None::<&ICoreWebView2AddScriptToExecuteOnDocumentCreatedCompletedHandler>,
                    )?;
                }
            }
            Ok(WebView { controller, view })
        }
    }

    pub fn navigate(&self, url: &str) {
        // SAFETY: COM call on the UI thread.
        let _ = unsafe { self.view.Navigate(&HSTRING::from(url)) };
    }

    /// Place the control, in physical pixels of the window's client area.
    pub fn set_bounds(&self, left: i32, top: i32, width: i32, height: i32) {
        let rect = RECT { left, top, right: left + width.max(0), bottom: top + height.max(0) };
        // SAFETY: COM call on the UI thread.
        let _ = unsafe { self.controller.SetBounds(rect) };
    }

    pub fn set_visible(&self, visible: bool) {
        // SAFETY: COM call on the UI thread.
        let _ = unsafe { self.controller.SetIsVisible(visible) };
    }

    /// Run a script in the page; `done` gets its result as JSON.
    pub fn execute(&self, script: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let done = Rc::new(RefCell::new(Some(done)));
        let finish = done.clone();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            if let Some(done) = finish.borrow_mut().take() {
                done(result.map(|()| json).map_err(|error| describe(&error)));
            }
            Ok(())
        }));
        // SAFETY: COM call on the UI thread.
        if let Err(error) = unsafe { self.view.ExecuteScript(&HSTRING::from(script), &handler) }
            && let Some(done) = done.borrow_mut().take()
        {
            done(Err(describe(&error)));
        }
    }

    /// Call a Chrome DevTools Protocol method; `done` gets the result object as JSON.
    pub fn devtools(&self, method: &str, params: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let done = Rc::new(RefCell::new(Some(done)));
        let finish = done.clone();
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
            if let Some(done) = finish.borrow_mut().take() {
                done(result.map(|()| json).map_err(|error| describe(&error)));
            }
            Ok(())
        }));
        // SAFETY: COM call on the UI thread.
        if let Err(error) = unsafe { self.view.CallDevToolsProtocolMethod(&HSTRING::from(method), &HSTRING::from(params), &handler) }
            && let Some(done) = done.borrow_mut().take()
        {
            done(Err(describe(&error)));
        }
    }

    /// Receive a DevTools Protocol event's parameters as JSON.
    pub fn on_devtools_event(&self, name: &str, mut handler: impl FnMut(String) + 'static) {
        let callback = DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut json = PWSTR::null();
                // SAFETY: COM call on the UI thread with an out-pointer WebView2 fills.
                if unsafe { args.ParameterObjectAsJson(&mut json) }.is_ok() {
                    handler(take_pwstr(json));
                }
            }
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: COM calls on the UI thread.
        let _ = unsafe { self.view.GetDevToolsProtocolEventReceiver(&HSTRING::from(name)).and_then(|receiver| receiver.add_DevToolsProtocolEventReceived(&callback, &mut token)) };
    }

    /// Run `handler` when a navigation finishes; receives the page's current URL and whether navigation succeeded.
    pub fn on_navigation_completed(&self, mut handler: impl FnMut(&str, bool) + 'static) {
        let callback = NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
            let mut ok = BOOL(0);
            if let Some(args) = args {
                // SAFETY: COM call on the UI thread with a valid out-pointer.
                let _ = unsafe { args.IsSuccess(&mut ok) };
            }
            let source = sender
                .and_then(|view| {
                    let mut uri = PWSTR::null();
                    // SAFETY: COM call on the UI thread with a valid out-pointer.
                    unsafe { view.Source(&mut uri) }.ok().map(|()| take_pwstr(uri))
                })
                .unwrap_or_default();
            handler(&source, ok.as_bool());
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: COM call on the UI thread.
        let _ = unsafe { self.view.add_NavigationCompleted(&callback, &mut token) };
    }

    /// Remove the control and its window.
    pub fn close(&self) {
        // SAFETY: COM call on the UI thread.
        let _ = unsafe { self.controller.Close() };
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn matches_exact_origin_only() {
        let origin = "http://127.0.0.1:47400";
        assert!(matches_origin("http://127.0.0.1:47400", origin));
        assert!(matches_origin("http://127.0.0.1:47400/", origin));
        assert!(matches_origin("http://127.0.0.1:47400/?view=pdf", origin));
        assert!(matches_origin("http://127.0.0.1:47400#section", origin));
        assert!(!matches_origin("http://127.0.0.1:474000/", origin));
        assert!(!matches_origin("http://127.0.0.1:47401/", origin));
        assert!(!matches_origin("http://127.0.0.1:47400@evil.com/", origin));
        assert!(!matches_origin("https://127.0.0.1:47400/", origin));
        assert!(!matches_origin("data:text/html,hi", origin));
        assert!(!matches_origin("blob:http://127.0.0.1:47400/abc", origin));
        assert!(!matches_origin("http://127.0.0.1:47400/", ""));
    }
}
