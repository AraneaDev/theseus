#[path = "support/browsing.rs"]
mod browsing;
#[allow(dead_code)]
#[path = "support/comparison.rs"]
mod comparison;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use theseus::{
    model::*,
    tui::{App, Focus, Tab, input, render},
};
fn key(app: &mut App, code: KeyCode) {
    input(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn screen(app: &App) -> String {
    screen_width(app, 160)
}
fn screen_width(app: &App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 60)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}
#[test]
fn end_reaches_records_past_u16_and_sparse_filter_last_match() {
    let mut app = App::offline(browsing::metadata_report(70_000, 0), "/tmp".into());
    app.tab = Tab::Files;
    app.focus = Focus::Evidence;
    key(&mut app, KeyCode::End);
    assert!(app.evidence_index > 65_535);
    assert!(screen(&app).contains("file-69999"));
    key(&mut app, KeyCode::Char('/'));
    for c in "file-69999".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    // Editing must leave the completed viewport/filter intact until commit.
    assert!(screen(&app).contains("Enter/Esc apply"));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::End);
    assert_eq!(app.evidence_index, 0);
    assert!(screen(&app).contains("file-69999"));
}
#[test]
fn text_chunks_reach_full_unicode_field_and_reset_on_context_width_and_revision() {
    let mut app = App::offline(browsing::metadata_report(2, 40_001), "/tmp".into());
    app.tab = Tab::Files;
    app.focus = Focus::Evidence;
    key(&mut app, KeyCode::End);
    assert!(!screen(&app).contains("DETAIL_TAIL"));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.scroll, 10);
    key(&mut app, KeyCode::PageUp);
    assert!(screen(&app).contains("DETAIL_TAIL"));
    key(&mut app, KeyCode::Char('['));
    assert!(!screen(&app).contains("DETAIL_TAIL"));
    key(&mut app, KeyCode::Char(']'));
    assert!(screen(&app).contains("DETAIL_TAIL"));
    assert!(!screen_width(&app, 100).contains("DETAIL_TAIL"));
    key(&mut app, KeyCode::Char(']'));
    app.root = "/elsewhere".into();
    assert!(!screen_width(&app, 100).contains("DETAIL_TAIL"));
    app.set_report(browsing::metadata_report(2, 0), "/tmp".into());
    assert!(!screen(&app).contains("DETAIL_TAIL"));
}
fn log_app(root: &std::path::Path, bytes: &[u8], truncated: bool) -> App {
    let store = theseus::report::create(root).unwrap();
    store
        .write_log("logs/out (stderr)", b"STDERR_ONLY", 100)
        .unwrap();
    store.write_log("logs/out", bytes, bytes.len()).unwrap();
    let mut report = browsing::metadata_report(0, 0);
    let mut cmd = CommandRecord::new("cmd", "exercise");
    cmd.stdout = Some(LogReference {
        path: "logs/out".into(),
        bytes: bytes.len() as u64,
        truncated,
    });
    cmd.stderr = Some(LogReference {
        path: "logs/out (stderr)".into(),
        bytes: 11,
        truncated: false,
    });
    report.scenarios[0].stages[0].commands.push(cmd);
    let mut app = App::offline(report, root.into());
    app.tab = Tab::Logs;
    app.focus = Focus::Evidence;
    app
}
#[test]
fn typed_log_chunks_reach_tail_and_keep_capture_labels_truthful() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = b"STDOUT_ONLY\n".to_vec();
    bytes.extend(vec![b'a'; 80_000]);
    bytes.extend(b"\nLOG_TAIL");
    let mut app = log_app(&temp.path().join("run"), &bytes, true);
    assert!(screen(&app).contains("STDOUT_ONLY"));
    assert!(!app.log_text.contains("STDERR_ONLY"));
    input(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    assert!(screen(&app).contains("LOG_TAIL"));
    assert!(app.log_text.contains("capture truncated"));
    assert!(!app.log_text.contains("complete stored stream"));
    key(&mut app, KeyCode::Char('['));
    assert!(!screen(&app).contains("LOG_TAIL"));
    key(&mut app, KeyCode::Char(']'));
    assert!(screen(&app).contains("LOG_TAIL"));
    input(
        &mut app,
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
    );
    assert!(screen(&app).contains("STDOUT_ONLY"));
    key(&mut app, KeyCode::Down);
    assert!(screen(&app).contains("STDERR_ONLY"));
}
#[test]
fn comparison_detail_focus_reaches_difference_101_and_full_source_field() {
    let temp = tempfile::tempdir().unwrap();
    let root = comparison::saved(temp.path());
    let mut doc = theseus::comparison::load_document(&root).unwrap();
    doc.report.cells[0].comparison.differences = (0..101)
        .map(|i| theseus::snapshot::FileChange {
            path: format!("item-{i:03}"),
            kind: theseus::snapshot::ChangeKind::Added,
            before: None,
            after: None,
        })
        .collect();
    doc.report.cells[0].comparison.differences[100].after = Some(theseus::snapshot::FileEntry {
        kind: FileKind::Symlink,
        mode: 0o777,
        uid: 10001,
        gid: 10001,
        size: 40_000,
        sha256: None,
        link_target: Some(format!("{}CHANGE_TAIL", "λ".repeat(20_000))),
    });
    doc.report.sources[0].image = Some(format!("{}SOURCE_TAIL", "x".repeat(1000)));
    let mut app = App::comparison_offline(doc, root);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::End);
    assert!(screen(&app).contains("item-100"));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::PageDown);
    key(&mut app, KeyCode::PageDown);
    assert!(screen(&app).contains("CHANGE_TAIL"));
    key(&mut app, KeyCode::Char('/'));
    for c in "SOURCE_TAIL".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Esc);
    assert!(screen(&app).contains("SOURCE_TAIL"));
}
fn body(app: &App) -> &str {
    app.log_text.split_once('\n').unwrap().1
}
#[test]
fn log_utf8_guards_preserve_scalars_invalid_bytes_tabs_and_raw_offsets() {
    for scalar in ["λ", "界", "🙂"] {
        let temp = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'a'; 4095];
        bytes.extend(scalar.as_bytes());
        bytes.extend(b"\t\x1b\xffEND");
        let mut app = log_app(&temp.path().join("run"), &bytes, false);
        app.refresh_logs();
        let first = body(&app).to_owned();
        assert_eq!(first, format!("{}{scalar}", "a".repeat(4095)));
        key(&mut app, KeyCode::Char(']'));
        assert_eq!(body(&app), "    ��END");
        assert!(app.log_text.contains(&format!(
            "raw bytes {}..{}",
            4095 + scalar.len(),
            bytes.len()
        )));
        key(&mut app, KeyCode::Char('['));
        assert_eq!(body(&app), first);
        key(&mut app, KeyCode::Char(']'));
        assert_eq!(body(&app), "    ��END");
    }
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = vec![b'a'; 4095];
    bytes.extend(b"\xf0\x9f\tEND\xe2\x82");
    let mut app = log_app(&temp.path().join("run"), &bytes, false);
    app.refresh_logs();
    let mut text = body(&app).to_owned();
    key(&mut app, KeyCode::Char(']'));
    text.push_str(body(&app));
    assert_eq!(text, format!("{}�    END�", "a".repeat(4095)));
}
#[test]
fn direct_tail_jump_and_previous_next_are_contiguous_for_unicode_and_controls() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = "🙂\t界\x1b\n".repeat(9000).into_bytes();
    let mut app = log_app(&temp.path().join("run"), &bytes, false);
    input(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    let tail = body(&app).to_owned();
    let start: usize = app
        .log_text
        .strip_prefix("raw bytes ")
        .unwrap()
        .split('.')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        tail,
        theseus::tui::safe_text(&String::from_utf8_lossy(&bytes[start..])).replace('\t', "    ")
    );
    key(&mut app, KeyCode::Char('['));
    let previous = body(&app).to_owned();
    let previous_start: usize = app
        .log_text
        .strip_prefix("raw bytes ")
        .unwrap()
        .split('.')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(app.log_text.contains(&format!("..{start} /")));
    assert_eq!(
        previous,
        theseus::tui::safe_text(&String::from_utf8_lossy(&bytes[previous_start..start]))
            .replace('\t', "    ")
    );
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(body(&app), tail);
    key(&mut app, KeyCode::Char('['));
    assert_eq!(body(&app), previous);
}
#[test]
fn log_replacement_short_storage_and_root_change_invalidate_pages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("run");
    let mut app = log_app(&root, &vec![b'a'; 20_000], true);
    input(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    std::fs::write(root.join("logs/out"), b"REPLACED").unwrap();
    let text = screen(&app);
    assert!(text.contains("REPLACED"));
    assert!(text.contains("incomplete current storage"));
    key(&mut app, KeyCode::Char('['));
    assert!(body(&app).contains("REPLACED"));
    app.root = temp.path().join("absent");
    assert!(screen(&app).contains("Cannot read log"));
}
#[test]
fn comparison_cell_heading_keeps_each_context_field_visible_with_a_long_variant() {
    let temp = tempfile::tempdir().unwrap();
    let root = comparison::saved(temp.path());
    let mut doc = theseus::comparison::load_document(&root).unwrap();
    doc.report.cells[0].variant_id = format!("VARIANT_LABEL{}", "x".repeat(3000));
    doc.report.cells[0].stage = "STAGE_LABEL".into();
    let mut app = App::comparison_offline(doc, root);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::End);
    let text = screen(&app);
    for field in ["VARIANT_LABEL", "STAGE_LABEL", "Scope app", "user/clean"] {
        assert!(text.contains(field), "heading omitted {field}");
    }
}
#[test]
fn width_and_context_reset_local_scroll_and_filter_commit_resets_chunks() {
    let mut report = browsing::metadata_report(1, 0);
    report.scenarios[0].stages[0]
        .errors
        .push(format!("PAGE_HEAD\n{}", "line\n".repeat(4000)));
    let mut app = App::offline(report, "/tmp".into());
    app.focus = Focus::Evidence;
    app.filter = "PAGE_HEAD".into();
    key(&mut app, KeyCode::End);
    assert_eq!(screen(&app).matches("PAGE_HEAD").count(), 2);
    key(&mut app, KeyCode::PageDown);
    assert_eq!(screen(&app).matches("PAGE_HEAD").count(), 1);
    assert_eq!(screen_width(&app, 140).matches("PAGE_HEAD").count(), 2);
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(screen_width(&app, 140).matches("PAGE_HEAD").count(), 1);
    key(&mut app, KeyCode::Char('/'));
    for c in "PAGE_HEAD".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Esc);
    assert_eq!(screen_width(&app, 140).matches("PAGE_HEAD").count(), 2);
    app.tab = Tab::Findings;
    assert_eq!(screen_width(&app, 140).matches("PAGE_HEAD").count(), 2);
}
#[test]
fn log_tail_input_before_first_draw_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = vec![b'a'; 80_000];
    bytes.extend(b"\nFIRST_DRAW_TAIL");
    let mut app = log_app(&temp.path().join("run"), &bytes, false);
    input(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    assert!(screen(&app).contains("FIRST_DRAW_TAIL"));
}
// Meter the real input path on this test's thread, excluding report/terminal
// construction. A repeated index scan or detail projection must allocate here.
thread_local! {
    static TRACK_ALLOCATIONS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCATED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
struct Meter;
unsafe impl std::alloc::GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|on| {
            if on.get() {
                ALLOCATED_BYTES.with(|n| n.set(n.get() + layout.size()));
            }
        });
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|on| {
            if on.get() {
                ALLOCATED_BYTES.with(|n| n.set(n.get() + size));
            }
        });
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;
#[test]
fn idle_input_reuses_completed_filter_index_and_detail_page() {
    let mut app = App::offline(browsing::metadata_report(70_000, 1_000_000), "/tmp".into());
    app.tab = Tab::Files;
    app.focus = Focus::Evidence;
    app.filter = "DETAIL_TAIL".into();
    key(&mut app, KeyCode::End);
    ALLOCATED_BYTES.with(|n| n.set(0));
    TRACK_ALLOCATIONS.with(|on| on.set(true));
    for _ in 0..5 {
        key(&mut app, KeyCode::Null);
    }
    TRACK_ALLOCATIONS.with(|on| on.set(false));
    let bytes = ALLOCATED_BYTES.with(std::cell::Cell::get);
    assert!(bytes < 1024, "cached input allocated {bytes} bytes");
    app.set_report(browsing::metadata_report(1, 0), "/tmp".into());
    assert!(screen(&app).contains("No evidence for this stage/filter"));
}
#[test]
fn log_projection_stays_bounded_with_tabs_and_matches_lossy_invalid_byte_decoding() {
    let temp = tempfile::tempdir().unwrap();
    let mut tabs = vec![b'\t'; 4095];
    tabs.extend("🙂TAIL".as_bytes());
    let mut app = log_app(&temp.path().join("tabs"), &tabs, false);
    app.refresh_logs();
    assert_eq!(body(&app), format!("{}🙂", " ".repeat(16_380)));
    assert_eq!(body(&app).len(), 16_384);
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(body(&app), "TAIL");
    let mut bytes = vec![b'a'; 4095];
    bytes.extend((0..=255).cycle().take(256 * 50));
    bytes.extend(b"\xf0\x9f");
    let mut app = log_app(&temp.path().join("invalid"), &bytes, false);
    app.refresh_logs();
    let mut actual = String::new();
    loop {
        assert!(body(&app).len() <= 16_384);
        actual.push_str(body(&app));
        if app.log_text.contains("last stored chunk") {
            break;
        }
        key(&mut app, KeyCode::Char(']'));
    }
    let expected = theseus::tui::safe_text(&String::from_utf8_lossy(&bytes)).replace('\t', "    ");
    assert_eq!(actual, expected);
}
#[test]
fn long_log_path_metadata_toggle_restores_raw_at_first_chunk_and_keeps_filter_typing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("run");
    let mut bytes = b"STDOUT_ONLY\n".to_vec();
    bytes.extend(vec![b'a'; 80_000]);
    bytes.extend(b"\nRAW_TAIL");
    let mut app = log_app(&root, &bytes, false);
    let path = format!(
        "logs/{}/PATH_TAIL",
        (0..20)
            .map(|i| format!("{i:02}-{}", "p".repeat(35)))
            .collect::<Vec<_>>()
            .join("/")
    );
    std::fs::create_dir_all(root.join(&path).parent().unwrap()).unwrap();
    std::fs::write(root.join(&path), &bytes).unwrap();
    let mut report = app.report.clone().unwrap();
    report.scenarios[0].stages[0].commands[0]
        .stdout
        .as_mut()
        .unwrap()
        .path = path;
    app.set_report(report, root);
    input(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    assert!(screen(&app).contains("RAW_TAIL"));
    key(&mut app, KeyCode::Char('m'));
    assert!(
        app.log_text.is_empty(),
        "metadata mode must not retain a raw mirror"
    );
    assert!(screen(&app).contains("PATH_TAIL"));
    assert!(screen(&app).contains("log metadata"));
    key(&mut app, KeyCode::Char('m'));
    assert!(screen(&app).contains("STDOUT_ONLY"));
    assert!(app.log_text.contains("raw bytes 0.."));
    assert!(!body(&app).contains("RAW_TAIL"));
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('m'));
    assert!(app.editing_filter);
    assert!(screen(&app).contains("m_"));
    assert!(app.log_text.contains("STDOUT_ONLY"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.filter, "m");
}
