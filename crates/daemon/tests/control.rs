use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use agent_master_client::{AgentMaster, Refusal, State, WindowSize};

mod harness;
use harness::*;

#[tokio::test]
async fn nothing_runs_until_something_is_started() {
    let mut master = master().await;
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn every_operation_says_so_before_a_process_exists() {
    let mut master = master().await;
    assert_eq!(refusal(master.window().await.expect_err("window")), Refusal::NothingStarted);
    assert_eq!(refusal(master.input(b"x").await.expect_err("input")), Refusal::NothingStarted);
    let size = WindowSize { rows: 10, cols: 40 };
    assert_eq!(refusal(master.resize(size).await.expect_err("resize")), Refusal::NothingStarted);
    let grace = Duration::from_secs(1);
    assert_eq!(refusal(master.stop(grace).await.expect_err("stop")), Refusal::NothingStarted);
    let refused = master.clipboard().await.expect_err("clipboard");
    assert_eq!(refusal(refused), Refusal::NothingStarted);
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn what_the_process_prints_lands_in_the_window() {
    let mut master = master().await;
    assert_eq!(master.start(shell("echo hello")).await.expect("start"), State::Running);
    window_showing(&mut master, "hello").await;
    assert_eq!(exit_code(&ended(&mut master).await), Some(0));
}

#[tokio::test]
async fn input_reaches_the_process_as_if_typed() {
    let mut master = master().await;
    master.start(shell("read line; echo \"got:$line\"")).await.expect("start");
    master.input(b"typed\r").await.expect("input");
    window_showing(&mut master, "got:typed").await;
}

#[tokio::test]
async fn control_bytes_reach_the_process_unchanged() {
    let mut master = master().await;
    master.start(shell("stty -echo; cat -v")).await.expect("start");
    master.input(b"\x1b[A\r").await.expect("input");
    window_showing(&mut master, "^[[A").await;
}

#[tokio::test]
async fn the_process_runs_in_the_given_directory() {
    let mut master = master().await;
    let cwd = std::env::temp_dir().join("agent-master-cwd-test");
    std::fs::create_dir_all(&cwd).expect("create the working directory");
    let mut spec = shell("basename \"$PWD\"");
    spec.cwd = cwd;
    master.start(spec).await.expect("start");
    window_showing(&mut master, "agent-master-cwd-test").await;
}

#[tokio::test]
async fn the_process_runs_with_the_given_environment() {
    let mut master = master().await;
    let mut spec = shell("echo \"seen:$AGENT_MASTER_TEST\"");
    spec.env = BTreeMap::from([("AGENT_MASTER_TEST".to_string(), "value".to_string())]);
    master.start(spec).await.expect("start");
    window_showing(&mut master, "seen:value").await;
}

#[tokio::test]
async fn the_environment_of_agent_master_itself_carries_over() {
    std::env::set_var("AGENT_MASTER_INHERITED", "from-the-daemon");
    let mut master = master().await;
    master.start(shell("echo \"seen:$AGENT_MASTER_INHERITED\"")).await.expect("start");
    window_showing(&mut master, "seen:from-the-daemon").await;
}

#[tokio::test]
async fn the_given_environment_wins_over_the_inherited_one() {
    std::env::set_var("AGENT_MASTER_OVERRIDDEN", "from-the-daemon");
    let mut master = master().await;
    let mut spec = shell("echo \"seen:$AGENT_MASTER_OVERRIDDEN\"");
    spec.env =
        BTreeMap::from([("AGENT_MASTER_OVERRIDDEN".to_string(), "from-the-spec".to_string())]);
    master.start(spec).await.expect("start");
    window_showing(&mut master, "seen:from-the-spec").await;
}

#[tokio::test]
async fn the_process_sees_the_size_it_was_started_with() {
    let mut master = master().await;
    let mut spec = shell("stty size");
    spec.size = WindowSize { rows: 30, cols: 100 };
    master.start(spec).await.expect("start");
    let window = master.window().await.expect("window");
    assert_eq!(window.size, WindowSize { rows: 30, cols: 100 });
    window_showing(&mut master, "30 100").await;
}

#[tokio::test]
async fn the_process_sees_a_resize() {
    let mut master = master().await;
    master.start(shell("read _; stty size")).await.expect("start");
    master.resize(WindowSize { rows: 30, cols: 100 }).await.expect("resize");
    master.input(b"\r").await.expect("input");
    let window = master.window().await.expect("window");
    assert_eq!(window.size, WindowSize { rows: 30, cols: 100 });
    window_showing(&mut master, "30 100").await;
}

#[tokio::test]
async fn a_window_too_small_to_draw_is_refused_at_the_start() {
    let mut master = master().await;
    for size in [
        WindowSize { rows: 0, cols: 0 },
        WindowSize { rows: 0, cols: 80 },
        WindowSize { rows: 24, cols: 0 },
        WindowSize { rows: 1, cols: 80 },
        WindowSize { rows: 24, cols: 1 },
    ] {
        let mut spec = shell("echo hello");
        spec.size = size;
        let refused = refusal(master.start(spec).await.expect_err("start"));
        assert_eq!(
            refused,
            Refusal::UnusableWindowSize { least: WindowSize { rows: 2, cols: 2 } },
            "{size:?}"
        );
        assert_eq!(master.state().await.expect("state"), State::NotStarted, "{size:?}");
    }
    let smallest = WindowSize { rows: 2, cols: 2 };
    let mut spec = shell("echo hello");
    spec.size = smallest;
    master.start(spec).await.expect("the smallest drawable window is allowed");
    assert_eq!(master.window().await.expect("window").size, smallest);
}

#[tokio::test]
async fn a_resize_to_a_window_too_small_to_draw_is_refused() {
    let mut master = master().await;
    master.start(shell("read _")).await.expect("start");
    let refused =
        refusal(master.resize(WindowSize { rows: 0, cols: 0 }).await.expect_err("resize"));
    assert_eq!(refused, Refusal::UnusableWindowSize { least: WindowSize { rows: 2, cols: 2 } });
    assert_eq!(master.window().await.expect("window").size, WindowSize { rows: 24, cols: 80 });
    assert_eq!(master.state().await.expect("state"), State::Running);
}

#[tokio::test]
async fn a_gentle_stop_lets_the_process_end_itself() {
    let mut master = master().await;
    master
        .start(shell("trap 'exit 7' TERM; echo ready; while :; do sleep 0.05; done"))
        .await
        .expect("start");
    window_showing(&mut master, "ready").await;
    let stopped = master.stop(Duration::from_secs(3)).await.expect("stop");
    assert_eq!(exit_code(&stopped), Some(7));
}

#[tokio::test]
async fn a_process_that_ignores_the_gentle_stop_is_killed() {
    let mut master = master().await;
    master
        .start(shell("trap '' TERM; echo ready; while :; do sleep 0.05; done"))
        .await
        .expect("start");
    window_showing(&mut master, "ready").await;
    let stopped = master.stop(Duration::from_millis(200)).await.expect("stop");
    assert!(matches!(stopped, State::Exited { signal: Some(libc::SIGKILL), .. }), "{stopped:?}");
}

#[tokio::test]
async fn no_grace_at_all_kills_the_process_outright() {
    let mut master = master().await;
    master
        .start(shell("trap '' TERM; echo ready; while :; do sleep 0.05; done"))
        .await
        .expect("start");
    window_showing(&mut master, "ready").await;
    let stopped = master.stop(Duration::ZERO).await.expect("stop");
    assert!(matches!(stopped, State::Exited { signal: Some(libc::SIGKILL), .. }), "{stopped:?}");
}

#[tokio::test]
async fn what_the_process_spawned_is_stopped_with_it() {
    let mut master = master().await;
    master
        .start(shell("sleep 300 & echo kid:$!; trap '' TERM; while :; do sleep 0.05; done"))
        .await
        .expect("start");
    let window = window_showing(&mut master, "kid:").await;
    let kid: i32 = window
        .split("kid:")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no child pid in the window:\n{window}"));
    assert_eq!(unsafe { libc::kill(kid, 0) }, 0, "the child was never alive");

    master.stop(Duration::from_millis(200)).await.expect("stop");
    let deadline = Instant::now() + PATIENCE;
    while unsafe { libc::kill(kid, 0) } == 0 {
        assert!(Instant::now() < deadline, "the process's own child outlived the stop");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_last_window_outlives_the_process() {
    let mut master = master().await;
    master.start(shell("echo farewell")).await.expect("start");
    let state = ended(&mut master).await;
    assert!(text(last_window(&state)).contains("farewell"), "{}", text(last_window(&state)));
    assert!(text(&master.window().await.expect("window")).contains("farewell"));
}

#[tokio::test]
async fn once_the_process_has_ended_there_is_only_one_window() {
    let mut master = master().await;
    master.start(shell("echo settled")).await.expect("start");
    let ending = ended(&mut master).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(&master.window().await.expect("window"), last_window(&ending));
    assert_eq!(last_window(&master.state().await.expect("state")), last_window(&ending));
}

#[tokio::test]
async fn a_process_whose_child_keeps_the_terminal_still_reports_its_end() {
    let mut master = master().await;
    master.start(shell("sleep 30 & exit 0")).await.expect("start");
    let state = tokio::time::timeout(Duration::from_secs(3), ended(&mut master))
        .await
        .expect("the end was never reported while a child held the terminal");
    assert_eq!(exit_code(&state), Some(0));
}

#[tokio::test]
async fn the_smallest_drawable_window_is_the_limit_for_a_resize_too() {
    let mut master = master().await;
    master.start(shell("read _")).await.expect("start");
    let smallest = WindowSize { rows: 2, cols: 2 };
    master.resize(smallest).await.expect("resize to the smallest drawable window");
    assert_eq!(master.window().await.expect("window").size, smallest);

    for size in [WindowSize { rows: 1, cols: 80 }, WindowSize { rows: 24, cols: 1 }] {
        let refused = refusal(master.resize(size).await.expect_err("resize"));
        assert_eq!(refused, Refusal::UnusableWindowSize { least: smallest }, "{size:?}");
        assert_eq!(master.window().await.expect("window").size, smallest, "{size:?}");
    }
    assert_eq!(master.state().await.expect("state"), State::Running);
}

#[tokio::test]
async fn shrinking_the_window_cuts_what_no_longer_fits() {
    let mut master = master().await;
    master.start(shell("printf 'MARKERTEXT'; read _")).await.expect("start");
    window_showing(&mut master, "MARKERTEXT").await;

    master.resize(WindowSize { rows: 2, cols: 4 }).await.expect("resize");
    let narrow = text(&master.window().await.expect("window"));
    assert!(narrow.starts_with("MARK"), "{narrow:?}");
    assert!(!narrow.contains("MARKERTEXT"), "{narrow:?}");

    master.resize(WindowSize { rows: 24, cols: 80 }).await.expect("resize");
    let wide = text(&master.window().await.expect("window"));
    assert!(!wide.contains("MARKERTEXT"), "what was cut cannot come back: {wide:?}");
}

#[tokio::test]
async fn input_at_the_very_moment_the_process_ends_is_refused_and_leaves_the_session_usable() {
    let mut master = master().await;
    master.start(shell("exit 0")).await.expect("start");
    let deadline = Instant::now() + PATIENCE;
    loop {
        match master.input(b"x").await {
            Ok(()) => assert!(Instant::now() < deadline, "the input never started failing"),
            Err(error) => {
                let refused = refusal(error);
                assert!(
                    matches!(refused, Refusal::AlreadyExited | Refusal::Failed { .. }),
                    "{refused:?}"
                );
                break;
            }
        }
    }
    assert!(matches!(ended(&mut master).await, State::Exited { .. }));
    assert!(master.window().await.is_ok());
}

#[tokio::test]
async fn a_connection_torn_down_mid_exchange_leaves_the_process_alone() {
    let address = listening().await;
    let mut starter = AgentMaster::connect(address).await.expect("connect");
    starter.start(shell("read _; echo survived")).await.expect("start");

    let rude = std::net::TcpStream::connect(address).expect("connect");
    let abrupt = libc::linger { l_onoff: 1, l_linger: 0 };
    unsafe {
        libc::setsockopt(
            std::os::fd::AsRawFd::as_raw_fd(&rude),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::addr_of!(abrupt).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
    drop(rude);

    let mut later = AgentMaster::connect(address).await.expect("reconnect");
    assert_eq!(later.state().await.expect("state"), State::Running);
    later.input(b"\r").await.expect("input");
    window_showing(&mut later, "survived").await;
}

#[tokio::test]
async fn an_input_larger_than_the_terminal_can_hold_arrives_whole() {
    let mut master = master().await;
    let counted = std::env::temp_dir().join("agent-master-large-input");
    let _ = std::fs::remove_file(&counted);
    let script = format!(
        "stty raw -echo; printf 'ready\\r\\n'; head -c 100000 > '{}'; printf 'counted\\r\\n'",
        counted.display()
    );
    master.start(shell(&script)).await.expect("start");
    window_showing(&mut master, "ready").await;

    master.input(&vec![b'x'; 100_000]).await.expect("input");
    window_showing(&mut master, "counted").await;
    assert_eq!(std::fs::read(&counted).expect("read what arrived").len(), 100_000);
}

#[tokio::test]
async fn a_second_process_is_refused_while_the_first_runs() {
    let mut master = master().await;
    master.start(shell("read _")).await.expect("start");
    let refused = refusal(master.start(shell("echo second")).await.expect_err("second start"));
    assert_eq!(refused, Refusal::AlreadyRunning);
}

#[tokio::test]
async fn a_process_can_be_started_once_the_previous_one_ended() {
    let mut master = master().await;
    master.start(shell("echo first")).await.expect("start");
    ended(&mut master).await;
    master.start(shell("echo second")).await.expect("start again");
    let window = window_showing(&mut master, "second").await;
    assert!(!window.contains("first"), "{window}");
}

#[tokio::test]
async fn an_ended_process_takes_no_input_and_no_resize() {
    let mut master = master().await;
    master.start(shell("echo done")).await.expect("start");
    ended(&mut master).await;
    assert_eq!(refusal(master.input(b"x").await.expect_err("input")), Refusal::AlreadyExited);
    let size = WindowSize { rows: 10, cols: 40 };
    assert_eq!(refusal(master.resize(size).await.expect_err("resize")), Refusal::AlreadyExited);
}

#[tokio::test]
async fn stopping_an_ended_process_reports_how_it_ended() {
    let mut master = master().await;
    master.start(shell("exit 3")).await.expect("start");
    ended(&mut master).await;
    assert_eq!(exit_code(&master.stop(Duration::from_secs(1)).await.expect("stop")), Some(3));
}

#[tokio::test]
async fn a_process_that_cannot_be_spawned_is_reported_and_leaves_nothing_behind() {
    let mut master = master().await;
    let missing = spec(&["/nonexistent/agent-master-test-binary"]);
    let refused = refusal(master.start(missing).await.expect_err("start"));
    assert!(
        matches!(&refused, Refusal::Failed { message } if message.contains("spawning")),
        "{refused:?}"
    );
    assert_eq!(master.state().await.expect("state"), State::NotStarted);
}

#[tokio::test]
async fn a_failed_start_leaves_the_previous_run_as_it_was() {
    let mut master = master().await;
    master.start(shell("echo before; exit 5")).await.expect("start");
    let before = ended(&mut master).await;

    let missing = spec(&["/nonexistent/agent-master-test-binary"]);
    master.start(missing).await.expect_err("start");

    let after = master.state().await.expect("state");
    assert_eq!(exit_code(&after), Some(5));
    assert_eq!(last_window(&before), last_window(&after));
}

#[tokio::test]
async fn a_large_input_does_not_hold_up_the_other_operations() {
    let address = listening().await;
    let mut starter = AgentMaster::connect(address).await.expect("connect");
    starter.start(spec(&["sleep", "300"])).await.expect("start");

    let mut typist = AgentMaster::connect(address).await.expect("connect");
    tokio::spawn(async move { typist.input(&vec![b'x'; 1_000_000]).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let answer = tokio::time::timeout(Duration::from_secs(2), starter.window());
    answer.await.expect("the window waited out the input").expect("window");
    let answer = tokio::time::timeout(Duration::from_secs(2), starter.state());
    answer.await.expect("the state waited out the input").expect("state");
    let answer = tokio::time::timeout(Duration::from_secs(2), starter.stop(Duration::ZERO));
    let stopped = answer.await.expect("the stop waited out the input").expect("stop");
    assert!(matches!(stopped, State::Exited { .. }), "{stopped:?}");
}

#[tokio::test]
async fn the_clipboard_is_empty_until_the_process_puts_something_on_it() {
    let mut master = master().await;
    master.start(shell("read _")).await.expect("start");
    assert_eq!(master.clipboard().await.expect("clipboard"), None);
}

#[tokio::test]
async fn what_the_process_copies_is_on_the_clipboard() {
    let mut master = master().await;
    let script = format!("{}; printf 'done\\n'", copying("first thing"));
    master.start(shell(&script)).await.expect("start");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(b"first thing".to_vec()));
}

#[tokio::test]
async fn a_copy_counts_whichever_buffer_and_ending_it_uses() {
    for (selection, terminator) in [("c", BELL), ("p", BELL), ("", BELL), ("c", STRING_TERMINATOR)]
    {
        let mut master = master().await;
        let copied = format!("into {selection:?}");
        let script = format!("{}; printf 'done\\n'", copying_to(selection, &copied, terminator));
        master.start(shell(&script)).await.expect("start");
        window_showing(&mut master, "done").await;
        assert_eq!(
            master.clipboard().await.expect("clipboard"),
            Some(copied.into_bytes()),
            "selection {selection:?}"
        );
    }
}

#[tokio::test]
async fn copying_nothing_is_not_the_same_as_copying_nothing_yet() {
    let mut master = master().await;
    let script = format!("{}; printf 'done\\n'", copying(""));
    master.start(shell(&script)).await.expect("start");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(Vec::new()));
}

#[tokio::test]
async fn a_copy_the_terminal_cannot_read_leaves_the_buffer_as_it_was() {
    let mut master = master().await;
    let unreadable = "printf '\\033]52;c;aGk\\007'";
    let script = format!("{}; read _; {unreadable}; printf 'done\\n'", copying("the good link"));
    master.start(shell(&script)).await.expect("start");
    master.input(b"\r").await.expect("input");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(b"the good link".to_vec()));
}

#[tokio::test]
async fn a_copy_the_terminal_cannot_read_leaves_an_empty_buffer_empty() {
    let mut master = master().await;
    let script = "printf '\\033]52;c;aGk\\007'; printf 'done\\n'; read _; printf 'still here\\n'";
    master.start(shell(script)).await.expect("start");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), None);

    master.input(b"\r").await.expect("input");
    window_showing(&mut master, "still here").await;
}

#[tokio::test]
async fn a_copy_too_big_for_one_read_arrives_whole() {
    let mut master = master().await;
    let huge = "abcdefghij".repeat(1_200);
    let script = format!("{}; printf 'done\\n'", copying(&huge));
    master.start(shell(&script)).await.expect("start");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(huge.into_bytes()));
}

#[tokio::test]
async fn a_later_copy_replaces_the_earlier_one() {
    let mut master = master().await;
    let script = format!(
        "{}; read _; {}; printf 'done\\n'",
        copying("first thing"),
        copying("second thing")
    );
    master.start(shell(&script)).await.expect("start");
    master.input(b"\r").await.expect("input");
    window_showing(&mut master, "done").await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(b"second thing".to_vec()));
}

#[tokio::test]
async fn what_was_copied_outlives_the_process() {
    let mut master = master().await;
    let script = format!("{}; exit 0", copying("outlives me"));
    master.start(shell(&script)).await.expect("start");
    ended(&mut master).await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(b"outlives me".to_vec()));
}

#[tokio::test]
async fn a_new_process_starts_with_an_empty_clipboard() {
    let mut master = master().await;
    master.start(shell(&format!("{}; exit 0", copying("from the first")))).await.expect("start");
    ended(&mut master).await;
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(b"from the first".to_vec()));

    master.start(shell("read _")).await.expect("start again");
    assert_eq!(master.clipboard().await.expect("clipboard"), None);
}

#[tokio::test]
async fn every_client_drives_the_same_process() {
    let address = listening().await;
    let mut first = AgentMaster::connect(address).await.expect("connect");
    let mut second = AgentMaster::connect(address).await.expect("connect");
    first.start(shell("read line; echo \"got:$line\"")).await.expect("start");
    assert_eq!(second.state().await.expect("state"), State::Running);
    second.input(b"shared\r").await.expect("input");
    window_showing(&mut first, "got:shared").await;
}

#[tokio::test]
async fn the_process_outlives_the_connection_that_started_it() {
    let address = listening().await;
    let mut starter = AgentMaster::connect(address).await.expect("connect");
    starter.start(shell("read line; echo \"got:$line\"")).await.expect("start");
    drop(starter);

    let mut later = AgentMaster::connect(address).await.expect("reconnect");
    assert_eq!(later.state().await.expect("state"), State::Running);
    later.input(b"still here\r").await.expect("input");
    window_showing(&mut later, "got:still here").await;
}

#[tokio::test]
async fn an_unreadable_request_is_answered_without_dropping_the_connection() {
    use agent_master_protocol::{read_message, write_message, Incoming, Request, Response};
    use tokio::io::{AsyncWriteExt, BufReader};

    let stream = tokio::net::TcpStream::connect(listening().await).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);

    write.write_all(b"not a request\n").await.expect("write");
    write_message(&mut write, &Request::State).await.expect("write");

    match read_message(&mut read).await.expect("read") {
        Incoming::Message(Response::Refused(refused)) => {
            assert!(matches!(refused, agent_master_protocol::Refusal::Unreadable { .. }));
        }
        other => panic!("expected a refusal, got {:?}", matches!(other, Incoming::Ended)),
    }
    match read_message(&mut read).await.expect("read") {
        Incoming::Message(Response::State(state)) => {
            assert_eq!(state, agent_master_protocol::State::NotStarted);
        }
        _ => panic!("the connection did not survive the unreadable request"),
    }
}
