use agent_master_client::{AgentMaster, Window, WindowSize};

mod harness;
use harness::*;

const LOGIN_URL: &str = "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference&code_challenge=n4bQgYhMfWWaL_qgxVrQFaO_TxsrC4Is0V1sFbDwCgg&code_challenge_method=S256&state=6ba7b810";

async fn window_of(master: &mut AgentMaster) -> Window {
    master.window().await.expect("window")
}

fn parsed(window: &Window) -> vt100::Parser {
    let mut parser = vt100::Parser::new(window.size.rows, window.size.cols, 0);
    parser.process(&window.contents);
    parser
}

#[tokio::test]
async fn a_line_wider_than_the_window_is_read_back_whole() {
    let mut master = master().await;
    master.start(shell(&format!("printf '%s\\n' '{LOGIN_URL}'"))).await.expect("start");
    let window = window_showing(&mut master, "https://claude.ai/oauth").await;

    let snapshot = window_of(&mut master).await;
    let rows: Vec<String> = parsed(&snapshot).screen().rows(0, snapshot.size.cols).collect();
    assert!(
        !rows.iter().any(|row| row.contains(LOGIN_URL)),
        "the link fitted on one row, so this proves nothing"
    );
    assert!(window.contains(LOGIN_URL), "the link came back broken:\n{window}");
}

#[tokio::test]
async fn a_hyperlink_leaves_the_text_around_it_alone() {
    let mut master = master().await;
    let script = format!(
        "printf '\\033]8;id=1;%s\\007click here\\033]8;;\\007 %s\\n' '{LOGIN_URL}' '{LOGIN_URL}'"
    );
    master.start(shell(&script)).await.expect("start");
    let window = window_showing(&mut master, "click here").await;

    assert!(window.contains(LOGIN_URL), "the visible link came back broken:\n{window}");
    assert!(!window.contains("id=1"), "the hyperlink's own parameters leaked in:\n{window}");
}

#[tokio::test]
async fn a_link_that_is_never_painted_is_not_in_the_window() {
    let mut master = master().await;
    let script =
        format!("printf '\\033]8;id=1;%s\\007open the page\\033]8;;\\007\\n' '{LOGIN_URL}'");
    master.start(shell(&script)).await.expect("start");
    let window = window_showing(&mut master, "open the page").await;

    assert!(!window.contains(LOGIN_URL), "{window}");
}

#[tokio::test]
async fn what_the_process_copies_is_not_in_the_window_either() {
    let mut master = master().await;
    let secret = "https://claude.ai/oauth/authorize?code=true";
    let script = format!("{}; printf 'copied\\n'", copying(secret));
    master.start(shell(&script)).await.expect("start");
    let window = window_showing(&mut master, "copied").await;

    assert!(!window.contains(secret), "{window}");
    assert_eq!(master.clipboard().await.expect("clipboard"), Some(secret.as_bytes().to_vec()));
}

#[tokio::test]
async fn the_window_shows_that_the_link_was_copied() {
    let mut master = master().await;
    let script = format!(
        "stty raw -echo; printf 'Press c to copy the link\\r\\n'; \
         key=$(dd bs=1 count=1 2>/dev/null); {}; printf 'Copied: %s\\r\\n' \"$key\"",
        copying(LOGIN_URL)
    );
    master.start(shell(&script)).await.expect("start");
    let before = window_showing(&mut master, "Press c to copy the link").await;
    assert!(!before.contains("Copied"), "{before}");
    assert_eq!(master.clipboard().await.expect("clipboard"), None);

    master.input(b"c").await.expect("input");
    let after = window_showing(&mut master, "Copied: c").await;
    assert!(!after.contains(LOGIN_URL), "the link landed in the window:\n{after}");
    assert_eq!(
        master.clipboard().await.expect("clipboard"),
        Some(LOGIN_URL.as_bytes().to_vec()),
        "the link should have reached the clipboard"
    );
}

#[tokio::test]
async fn words_a_process_places_by_column_stay_apart() {
    let mut master = master().await;
    master
        .start(shell("printf 'Choose\\033[9Gthe\\033[13Gtext\\033[18Gstyle\\n'"))
        .await
        .expect("start");
    let window = window_showing(&mut master, "Choose").await;
    assert!(window.contains("Choose  the text style"), "{window:?}");
}

#[tokio::test]
async fn colours_and_the_cursor_survive_the_snapshot() {
    let mut master = master().await;
    master.start(shell("printf '\\033[31mRED\\033[0m'")).await.expect("start");
    window_showing(&mut master, "RED").await;

    let screen = parsed(&window_of(&mut master).await);
    let screen = screen.screen();
    assert_eq!(screen.cell(0, 0).expect("a painted cell").fgcolor(), vt100::Color::Idx(1));
    assert_eq!(screen.cursor_position(), (0, 3));
}

#[tokio::test]
async fn text_split_across_reads_is_not_broken() {
    let mut master = master().await;
    master
        .start(shell("i=0; while [ $i -lt 200 ]; do printf 'ярусыпорядок'; i=$((i+1)); done; printf '\\nмаркер\\n'"))
        .await
        .expect("start");
    let window = window_showing(&mut master, "маркер").await;
    assert!(!window.contains('\u{fffd}'), "a character was torn in half:\n{window}");
    assert!(window.contains("ярусыпорядок"), "{window}");
}

#[tokio::test]
async fn double_width_characters_survive() {
    let mut master = master().await;
    master.start(shell("printf '日本語テキスト\\n'")).await.expect("start");
    let window = window_showing(&mut master, "日本語").await;
    assert!(window.contains("日本語テキスト"), "{window}");
}

#[tokio::test]
async fn what_a_repaint_clears_is_gone_from_the_window() {
    let mut master = master().await;
    master
        .start(shell("printf 'A-VERY-LONG-STALE-LINE-THAT-MUST-NOT-SURVIVE\\n'; read _; printf '\\033[2J\\033[Hclean\\n'"))
        .await
        .expect("start");
    window_showing(&mut master, "STALE").await;
    master.input(b"\r").await.expect("input");
    let window = window_showing(&mut master, "clean").await;
    assert!(!window.contains("STALE"), "the cleared line is still in the window:\n{window}");
}

#[tokio::test]
async fn the_window_follows_the_process_onto_the_alternate_screen_and_back() {
    let mut master = master().await;
    master
        .start(shell(
            "printf 'PRIMARY\\n'; read _; printf '\\033[?1049hALTERNATE\\n'; read _; printf '\\033[?1049l'; read _",
        ))
        .await
        .expect("start");
    window_showing(&mut master, "PRIMARY").await;

    master.input(b"\r").await.expect("input");
    let window = window_showing(&mut master, "ALTERNATE").await;
    assert!(!window.contains("PRIMARY"), "the primary screen showed through:\n{window}");

    master.input(b"\r").await.expect("input");
    let window = window_showing(&mut master, "PRIMARY").await;
    assert!(!window.contains("ALTERNATE"), "the alternate screen stayed behind:\n{window}");
}

#[tokio::test]
async fn output_far_larger_than_one_read_arrives_in_order() {
    let mut master = master().await;
    master
        .start(shell("i=1; while [ $i -le 4000 ]; do echo \"line-$i\"; i=$((i+1)); done"))
        .await
        .expect("start");
    let window = window_showing(&mut master, "line-4000").await;

    let tail: Vec<&str> = window.lines().filter(|line| line.starts_with("line-")).collect();
    let last: Vec<&str> = tail[tail.len() - 3..].to_vec();
    assert_eq!(last, vec!["line-3998", "line-3999", "line-4000"], "{window}");
}

#[tokio::test]
async fn a_resize_makes_the_process_repaint_at_the_new_width() {
    let mut master = master().await;
    master.start(shell("while :; do read _ || exit 0; stty size; done")).await.expect("start");
    master.input(b"\r").await.expect("input");
    window_showing(&mut master, "24 80").await;

    master.resize(WindowSize { rows: 24, cols: 132 }).await.expect("resize");
    master.input(b"\r").await.expect("input");
    let window = window_showing(&mut master, "24 132").await;
    assert_eq!(window_of(&mut master).await.size, WindowSize { rows: 24, cols: 132 }, "{window}");
}

#[tokio::test]
async fn the_window_at_the_end_holds_what_the_process_printed_last() {
    let mut master = master().await;
    master.start(shell("printf 'last-word-before-the-end\\n'; exit 0")).await.expect("start");
    let state = ended(&mut master).await;
    let window = text(last_window(&state));
    assert!(window.contains("last-word-before-the-end"), "{window}");
    assert_eq!(exit_code(&state), Some(0));
}
