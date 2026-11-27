use ::core::prelude::v1::test;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use super::config::parse_ssh_config_with_home;
use super::exec::{LOADER, configured_remote_command, has_configured_remote_command, with_options, with_options_mode};
use super::runtime::{installer_answer, installer_args, installer_home, is_port_conflict_error, probe_answer, tail};
use super::*;
use crate::HostCommand;
use crate::targets::SshHostProfile;

#[test]
fn destinations() {
    assert_eq!(parse_destination("ssh dev@example.com:2222", None).unwrap(), Destination { destination: "dev@example.com".into(), port: Some(2222) });
    assert_eq!(parse_destination("SSH dev@example.com", None).unwrap(), Destination { destination: "dev@example.com".into(), port: None });
    assert_eq!(parse_destination("box", Some(22)).unwrap(), Destination { destination: "box".into(), port: None });
    assert_eq!(parse_destination("[::1]", None).unwrap(), Destination { destination: "[::1]".into(), port: None });
    assert_eq!(parse_destination("[::1]:2222", None).unwrap(), Destination { destination: "[::1]".into(), port: Some(2222) });
    assert_eq!(
        parse_destination("alice@[2001:db8::1]:2200", None).unwrap(),
        Destination { destination: "alice@[2001:db8::1]".into(), port: Some(2200) }
    );
    assert!(parse_destination("[::1]:70000", None).is_err());
    assert!(parse_destination("host:0", None).is_err());
    assert!(parse_destination("host:700000", None).is_err());
    assert!(parse_destination("-oProxyCommand=x", None).is_err());
    assert!(parse_destination("a b", None).is_err());
    assert!(parse_destination("host", Some(70000)).is_err());
    assert_eq!(name(&Destination { destination: "box".into(), port: Some(2200) }), "box:2200");
    assert!(matches!(target(&Destination { destination: "box".into(), port: None }), ExecutionTarget::Ssh { ref id, .. } if id == "ssh:box"));
}

#[test]
fn detects_remote_command_without_tripping_on_none() {
    assert!(!has_configured_remote_command("hostname example.com\nremotecommand none\n"));
    assert!(!has_configured_remote_command("remotecommand   NONE\n"));
    assert!(!has_configured_remote_command("remotecommand \n"));
    assert!(has_configured_remote_command("hostname example.com\nremotecommand tmux attach\n"));
}

#[test]
fn matches_localized_and_english_port_conflicts() {
    assert!(is_port_conflict_error("Could not request local forwarding."));
    assert!(is_port_conflict_error("bind: Address already in use"));
    assert!(is_port_conflict_error("端口 12345 已被使用"));
    assert!(is_port_conflict_error("SSH 端口转发失败"));
    assert!(!is_port_conflict_error("Permission denied (publickey)."));
}

#[cfg(unix)]
#[test]
fn the_timeout_stops_a_command_that_closed_its_output() {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "exec >&- 2>&-; sleep 30"]);
    let started = std::time::Instant::now();
    let output = super::exec::run_command(command, None, &mut |_| {}, Some(Duration::from_millis(300))).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    assert_eq!(output.code, None);
}

#[test]
fn config_aliases() {
    let text = "# Host ignored-line\nHost dev box # work server\n  HostName 10.0.0.2\nHost *.internal !bad\nhost   prod #comment trailing\nHost edge#inline extra\nHostName x\n";
    assert_eq!(config_hosts(text), ["dev", "box", "prod", "edge"]);
}

#[test]
fn batch_mode_disables_interactive_prompts_and_tracks_remote_host() {
    let target = Destination { destination: "dev@box".into(), port: Some(2222) };
    let args = with_options_mode(&target, &["true".into()], None, false, true, 10, (10, 3));
    assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "KbdInteractiveAuthentication=no"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "PasswordAuthentication=no"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "ConnectTimeout=10"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveInterval=10"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveCountMax=3"]));
    let parameters = json!({ "host": "/home/u/.rainy-agent/runtime/abc/rainy-host", "env": {} }).to_string();
    let mut cmd_args = vec!["-F".to_owned(), "-p".to_owned()]; // ensures option values named `-p` are skipped
    cmd_args.extend(with_options(&target, &remote_python(HOST_LAUNCHER, &[&parameters]), Some(5000), false));
    let command = HostCommand {
        program: PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe"),
        args: cmd_args.into_iter().map(OsString::from).collect(),
        env: Vec::new(),
        cwd: None,
        scrub_environment: false,
        wait_for_loopback: true,
        expected_port: Some(5000),
    };
    assert_eq!(
        host_of(&command),
        Some(RemoteHost {
            target: target.clone(),
            host: "/home/u/.rainy-agent/runtime/abc/rainy-host".into(),
        })
    );
    assert_eq!(host_of(&HostCommand { program: PathBuf::from("wsl.exe"), ..command.clone() }), None);
    let other = HostCommand {
        args: with_options(&target, &remote_python("print(1)", &[]), None, false).into_iter().map(OsString::from).collect(),
        ..command
    };
    assert_eq!(host_of(&other), None);
}

#[test]
fn parses_zero_upload_probe_answers() {
    assert_eq!(
        probe_answer("motd\n{\"host\": \"/h/.rainy-agent/runtime/x/rainy-host\"}\n").unwrap(),
        RuntimeProbe::Ready { host: "/h/.rainy-agent/runtime/x/rainy-host".into() }
    );
    assert_eq!(
        probe_answer("{\"needsApp\": true, \"needsBase\": false}\n").unwrap(),
        RuntimeProbe::NeedsInstall { needs_app: true, needs_base: false }
    );
    assert_eq!(
        probe_answer("{\"needsApp\": true, \"needsBase\": true}\n").unwrap(),
        RuntimeProbe::NeedsInstall { needs_app: true, needs_base: true }
    );
    assert_eq!(
        probe_answer("{\"unsupported\": \"Linux aarch64\"}\n").unwrap(),
        RuntimeProbe::Unsupported("Linux aarch64".into())
    );
    assert!(probe_answer("{\"host\": \"relative\"}").is_err());
    assert!(probe_answer("garbage").is_err());
}

#[test]
fn remote_words() {
    let words = remote_python("print(1)", &["a b"]);
    assert_eq!(&words[..3], ["python3", "-c", LOADER]);
    assert!(!LOADER[1..LOADER.len() - 1].contains(['\'', '\\', '$', '!']));
    let engine = base64::engine::general_purpose::STANDARD;
    assert_eq!(engine.decode(&words[4]).unwrap(), b"a b");
    let target = Destination { destination: "box".into(), port: Some(2200) };
    let args = with_options(&target, &words, Some(5000), false);
    assert!(args.windows(2).any(|pair| pair == ["-p", "2200"]));
    assert!(args.windows(2).any(|pair| pair == ["-L", "127.0.0.1:5000:127.0.0.1:5000"]));
    assert_eq!(args.iter().position(|arg| arg == "--").map(|at| &args[at + 1]), Some(&"box".to_owned()));
    assert!(!args.iter().any(|arg| arg.starts_with("RemoteCommand")));
    let args = with_options(&target, &words, None, true);
    let at = args.windows(2).position(|pair| pair == ["-o", "RemoteCommand=none"]).unwrap();
    assert!(at < args.iter().position(|arg| arg == "--").unwrap());
}

#[test]
#[ignore = "needs an SSH server"]
fn runs_on_a_host_whose_config_has_a_remote_command() {
    let host = std::env::var("RAINY_TEST_SSH_HOST").unwrap();
    let target = Destination { destination: host, port: None };
    assert!(configured_remote_command(&target));
    let output = run(&target, &remote_python("import sys\nprint('ok', sys.stdin.isatty())\n", &[]), &[], None, &mut |_| {}, Some(Duration::from_secs(60))).unwrap();
    assert_eq!((output.code, output.stdout.trim()), (Some(0), "ok False"), "{}", output.stderr);
}

#[test]
fn reads_installer_answers() {
    let ready = installer_answer("noise\n{\"host\": \"/h/r/rainy-host\"}\n").unwrap();
    assert_eq!(ready, ServerRuntime::Ready { host: "/h/r/rainy-host".into() });
    assert_eq!(installer_answer("{\"needsArchive\": true}").unwrap(), ServerRuntime::NeedsArchive { error: None });
    assert_eq!(
        installer_answer("{\"needsArchive\": true, \"error\": \"URLError: timed out\"}").unwrap(),
        ServerRuntime::NeedsArchive { error: Some("URLError: timed out".into()) }
    );
    assert_eq!(installer_answer("{\"unsupported\": \"Linux aarch64\"}").unwrap(), ServerRuntime::Unsupported("Linux aarch64".into()));
    assert!(installer_answer("not json").is_err());
    let noise = format!("{{\"host\": \"/h\"}}\n{}", "欢迎使用服务器。".repeat(40));
    for skip in 0..3 {
        let error = installer_answer(&noise[..noise.len() - skip * 3]).unwrap_err().to_string();
        assert!(error.contains("欢迎") && error.len() < 500, "{error}");
    }
    assert_eq!(tail("ab中", 2), "");
    assert_eq!(tail("ab中", 3), "中");
    let target = Destination { destination: "box".into(), port: None };
    assert_eq!(installed_paths(&target, ready, false).unwrap(), "/h/r/rainy-host");
    assert!(installed_paths(&target, ServerRuntime::NeedsArchive { error: None }, true).is_err());
}

#[test]
fn explains_failures() {
    let target = Destination { destination: "box".into(), port: None };
    let message = failure(&target, Some(255), "Warning: Permanently added x\nuser@box: Permission denied (publickey).").to_string();
    assert!(message.starts_with(&crate::locale::localized("box 拒绝了登录。", "box refused the login.")), "{message}");
    assert!(message.ends_with("Permission denied (publickey)."));
    assert!(!message.contains("Permanently added"));
    let missing_py = failure(&target, Some(127), "").to_string();
    assert!(missing_py.contains("python3"), "{missing_py}");
    crate::locale::set_chinese(true);
    assert!(login_refused("user@box: Permission denied (publickey,password)."));
    assert!(login_refused("Permission denied, please try again.\nuser@box: Permission denied (password)."));
    let traceback = "Traceback (most recent call last):\n  File \"<string>\", line 1\nPermissionError: [Errno 13] Permission denied: '/home/u/.rainy-agent'";
    assert!(!login_refused(traceback));
    let message = failure(&target, Some(1), traceback).to_string();
    assert!(message.ends_with("PermissionError: [Errno 13] Permission denied: '/home/u/.rainy-agent'"), "{message}");
    assert!(message.starts_with("Rainy 在 box 上") || message.starts_with("Rainy failed to install"), "{message}");
}

#[test]
fn parses_recursive_include_globs_quotes_comments_and_summaries() {
    let root = tempfile::tempdir().unwrap();
    let ssh_dir = root.path().join("ssh");
    let conf_d = ssh_dir.join("conf.d");
    let spaced = ssh_dir.join("my hosts");
    std::fs::create_dir_all(&conf_d).unwrap();
    std::fs::create_dir_all(&spaced).unwrap();

    let main_config = ssh_dir.join("config");
    std::fs::write(
        &main_config,
        r#"
# Top comment
Include conf.d/*.conf "my hosts/extra #1.conf" # trailing comment
Host=direct-box
  HostName=10.1.1.9
  Port=2200

Host *
  User fallback-user
  Port 2222
"#,
    )
    .unwrap();

    std::fs::write(
        conf_d.join("01-gpu.conf"),
        r#"
Host leaper
  HostName 192.168.10.20
  User root
  Port 22022
  ProxyJump bastion # jump host

Match host leaper
  User should-not-override
"#,
    )
    .unwrap();

    std::fs::write(
        conf_d.join("02-r5090.conf"),
        r#"
Host r5090 5090
  HostName 100.64.0.5
"#,
    )
    .unwrap();

    std::fs::write(
        spaced.join("extra #1.conf"),
        r#"
Host ncu-remote
  HostName 172.16.0.8
  User researcher
"#,
    )
    .unwrap();

    let summaries = parse_ssh_config(&main_config);
    let aliases: Vec<&str> = summaries.iter().map(|s| s.alias.as_str()).collect();
    assert_eq!(aliases, ["leaper", "r5090", "5090", "ncu-remote", "direct-box"]);

    let leaper = &summaries[0];
    assert_eq!(leaper.host_name.as_deref(), Some("192.168.10.20"));
    assert_eq!(leaper.user.as_deref(), Some("root"));
    assert_eq!(leaper.port, Some(22022));
    assert_eq!(leaper.proxy_jump.as_deref(), Some("bastion"));

    let r5090 = &summaries[1];
    assert_eq!(r5090.host_name.as_deref(), Some("100.64.0.5"));
    assert_eq!(r5090.user.as_deref(), Some("fallback-user"));
    assert_eq!(r5090.port, Some(2222));

    let direct = &summaries[4];
    assert_eq!(direct.host_name.as_deref(), Some("10.1.1.9"));
    assert_eq!(direct.port, Some(2200));
    assert_eq!(direct.user.as_deref(), Some("fallback-user"));
}

#[test]
fn include_stops_cycles_depth_overflow_and_path_traversal() {
    let root = tempfile::tempdir().unwrap();
    let ssh_dir = root.path().join("ssh");
    std::fs::create_dir_all(&ssh_dir).unwrap();

    let outside = root.path().join("secret.conf");
    std::fs::write(&outside, "Host leaked-host\n  HostName 1.2.3.4\n").unwrap();

    std::fs::write(ssh_dir.join("a.conf"), "Include b.conf\nHost alpha\n").unwrap();
    std::fs::write(ssh_dir.join("b.conf"), "Include a.conf\nHost beta\n").unwrap();

    for depth in 0..=10 {
        let mut body = format!("Host depth-{depth}\n");
        if depth < 10 {
            body.push_str(&format!("Include d{}.conf\n", depth + 1));
        }
        std::fs::write(ssh_dir.join(format!("d{depth}.conf")), body).unwrap();
    }

    let inside_abs_file = ssh_dir.join("inside-abs.conf");
    std::fs::write(&inside_abs_file, "Host inside-abs-ok\n").unwrap();
    let inside_abs = inside_abs_file.display().to_string();

    let outside_abs = outside.display().to_string();
    let main_config = ssh_dir.join("config");
    std::fs::write(
        &main_config,
        format!("Include ../secret.conf\nInclude \"{outside_abs}\"\nInclude \"{inside_abs}\"\nInclude a.conf\nInclude d0.conf\nHost local-ok\n"),
    )
    .unwrap();

    let summaries = parse_ssh_config(&main_config);
    let aliases: Vec<&str> = summaries.iter().map(|s| s.alias.as_str()).collect();
    assert!(!aliases.contains(&"leaked-host"), "traversal must be rejected: {aliases:?}");
    assert!(aliases.contains(&"inside-abs-ok"));
    assert!(aliases.contains(&"alpha"));
    assert!(aliases.contains(&"beta"));
    assert!(aliases.contains(&"depth-0"));
    assert!(aliases.contains(&"depth-7"));
    assert!(!aliases.contains(&"depth-9"), "depth beyond limit must be capped: {aliases:?}");
    assert!(aliases.contains(&"local-ok"));
}

#[test]
fn real_dot_ssh_config_allows_absolute_includes_within_home() {
    let fake_home = tempfile::tempdir().unwrap();
    let dot_ssh = fake_home.path().join(".ssh");
    let colima = fake_home.path().join(".colima");
    std::fs::create_dir_all(&dot_ssh).unwrap();
    std::fs::create_dir_all(&colima).unwrap();
    let colima_cfg = colima.join("ssh_config");
    std::fs::write(&colima_cfg, "Host colima-vm\n  HostName 127.0.0.1\n").unwrap();
    let main_config = dot_ssh.join("config");
    std::fs::write(&main_config, format!("Include \"{}\"\n", colima_cfg.display())).unwrap();
    let summaries = parse_ssh_config_with_home(&main_config, &dot_ssh, Some(fake_home.path()));
    assert!(summaries.iter().any(|s| s.alias == "colima-vm"), "{summaries:?}");
}

#[test]
fn include_preserves_enclosing_host_options_and_allows_shared_reuse() {
    let root = tempfile::tempdir().unwrap();
    let ssh_dir = root.path().join("ssh");
    std::fs::create_dir_all(&ssh_dir).unwrap();

    std::fs::write(ssh_dir.join("shared.conf"), "ProxyJump lab-jump\n").unwrap();
    std::fs::write(ssh_dir.join("top.conf"), "Host top-box\n  HostName 10.0.0.1\n").unwrap();
    let main_config = ssh_dir.join("config");
    std::fs::write(
        &main_config,
        r#"
Include top.conf
Port 2201

Host alpha
  Include shared.conf
  User alpha-user
  HostName 10.0.0.2

Host beta
  Include shared.conf
  User beta-user
  HostName 10.0.0.3
"#,
    )
    .unwrap();

    let summaries = parse_ssh_config(&main_config);
    let by_alias = |name: &str| summaries.iter().find(|s| s.alias == name).cloned().unwrap();

    let top = by_alias("top-box");
    assert_eq!(top.port, Some(2201));

    let alpha = by_alias("alpha");
    assert_eq!(alpha.proxy_jump.as_deref(), Some("lab-jump"));
    assert_eq!(alpha.user.as_deref(), Some("alpha-user"));
    assert_eq!(alpha.host_name.as_deref(), Some("10.0.0.2"));
    assert_eq!(alpha.port, Some(2201));

    let beta = by_alias("beta");
    assert_eq!(beta.proxy_jump.as_deref(), Some("lab-jump"));
    assert_eq!(beta.user.as_deref(), Some("beta-user"));
    assert_eq!(beta.host_name.as_deref(), Some("10.0.0.3"));
    assert_eq!(beta.port, Some(2201));
}

#[test]
fn profile_forwards_env_keepalive_and_keeps_host_of_intact() {
    use std::collections::BTreeMap;
    let target = Destination { destination: "leaper".into(), port: Some(2222) };
    let profile = SshHostProfile {
        rainy_home: Some("/root/kube-user/.rainy-agent".into()),
        env: BTreeMap::from([("HF_ENDPOINT".into(), "https://hf-mirror.com".into())]),
        keepalive_interval: Some(7),
        keepalive_count_max: Some(5),
        notes: Some("2号卡常被占".into()),
        ..Default::default()
    };
    let command = host_command_with_profile(
        &target,
        "/root/kube-user/.rainy-agent/runtime/abc/rainy-host",
        &[("RAINY_EXECUTION_TARGET_ID".into(), "ssh:leaper:2222".into())],
        5432,
        &[],
        Some(&profile),
    )
    .unwrap();
    let args: Vec<String> = command.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveInterval=7"]));
    assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveCountMax=5"]));
    assert_eq!(
        host_of(&command),
        Some(RemoteHost {
            target: target.clone(),
            host: "/root/kube-user/.rainy-agent/runtime/abc/rainy-host".into(),
        })
    );
    let sep = args.iter().position(|arg| arg == "--").unwrap();
    let params_b64 = &args[sep + 6];
    let engine = base64::engine::general_purpose::STANDARD;
    let params: Value = serde_json::from_slice(&engine.decode(params_b64).unwrap()).unwrap();
    assert_eq!(params["env"]["RAINY_HOME"], "/root/kube-user/.rainy-agent");
    assert_eq!(params["env"]["RAINY_SSH_NOTES"], "2号卡常被占");
    assert_eq!(params["env"]["RAINY_PORT"], "5432");
    let extra: BTreeMap<String, String> = serde_json::from_str(params["env"]["RAINY_SSH_EXTRA_ENV"].as_str().unwrap()).unwrap();
    assert_eq!(extra.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));

    let home = installer_home(Some(&profile));
    assert_eq!(home.as_deref(), Some("/root/kube-user/.rainy-agent"));
    assert_eq!(
        installer_args(home.as_deref(), "--probe", "{}", &["--prune"]),
        ["--home", "/root/kube-user/.rainy-agent", "--probe", "{}", "--prune"]
    );
    assert_eq!(installer_args(None, "--probe", "{}", &[]), ["--probe", "{}"]);
}
