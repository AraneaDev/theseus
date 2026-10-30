//! Measure allocation sizes on this test thread only; fixture storage is pre-existing.
#[path = "support/browsing.rs"]
mod browsing;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
thread_local! { static TRACK: Cell<bool> = const {Cell::new(false)}; static LARGEST: Cell<usize> = const {Cell::new(0)}; }
struct Meter;
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        TRACK.with(|on| {
            if on.get() {
                LARGEST.with(|n| n.set(n.get().max(l.size())))
            }
        });
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        TRACK.with(|on| {
            if on.get() {
                LARGEST.with(|m| m.set(m.get().max(n)))
            }
        });
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOC: Meter = Meter;
fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|n| n.set(0));
    TRACK.with(|c| c.set(true));
    let value = f();
    TRACK.with(|c| c.set(false));
    (value, LARGEST.with(Cell::get))
}
#[test]
fn large_field_projection_never_allocates_the_whole_formatted_record() {
    use theseus::tui::{
        App, Tab,
        evidence::{Catalog, FilterIndex},
        paging::DETAIL_BYTES,
    };
    let mut app = App::offline(
        browsing::metadata_report(70_001, 1_000_000),
        "/tmp/synthetic".into(),
    );
    app.tab = Tab::Files;
    let catalog = Catalog::for_app(&app);
    let (count, max) = measured(|| catalog.count());
    assert_eq!(count, 70_003);
    assert!(max < 1024, "count allocated {max}");
    let record = catalog.get(count - 1).unwrap().record;
    let offset = record.text().find('λ').unwrap() + 900_000;
    let (page, max) = measured(|| record.page(offset, DETAIL_BYTES).unwrap());
    assert_eq!(page.text.len(), DETAIL_BYTES);
    assert!(max <= DETAIL_BYTES, "page allocated {max}");
    let (_, max) = measured(|| FilterIndex::new(&catalog, "detail_tail"));
    assert!(max <= DETAIL_BYTES, "filter allocated {max}");
    let (_, max) = measured(|| FilterIndex::new(&catalog, "ς"));
    assert!(max <= DETAIL_BYTES, "contextual filter allocated {max}");
}
