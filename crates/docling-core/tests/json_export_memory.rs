//! Peak heap of `export_to_json_value` on a long document.
//!
//! The export's `json!` constructors deep-copy every interpolated `Value`
//! (`to_value`): the top-level object copied the finished `texts` array whole,
//! so the export briefly held its result twice — 22 GB at peak for a 10 GB
//! value on a 3M-item PDF. The hot constructors now move their parts in.
//!
//! Measured with a counting allocator, per thread so the harness's other test
//! threads don't leak into the numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use docling_core::{DoclingDocument, Node};

struct PerThread;

thread_local! {
    static CUR: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = CUR.try_with(|c| {
        let v = c.get() + delta;
        c.set(v);
        let _ = PEAK.try_with(|p| p.set(p.get().max(v)));
    });
}

unsafe impl GlobalAlloc for PerThread {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        track(l.size() as isize);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        track(-(l.size() as isize));
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        track(n as isize - l.size() as isize);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: PerThread = PerThread;

/// `pages` pages of `per_page` short located paragraphs — the flat-node shape
/// the PDF text layer produces.
fn long_document(pages: usize, per_page: usize) -> DoclingDocument {
    let mut doc = DoclingDocument::new("long");
    for p in 1..=pages {
        doc.nodes.push(Node::PageInfo {
            page_no: p,
            width: 612.0,
            height: 792.0,
        });
        for i in 0..per_page {
            let (x, y) = ((i % 10) as f32 * 50.0, (i / 10) as f32 * 12.0);
            doc.nodes.push(Node::Prov {
                page_no: p,
                bbox: [x, y, x + 40.0, y + 9.0],
                charspan: [0, 6],
                seq: None,
                inner: Box::new(Node::Paragraph {
                    text: format!("{:06}", p * per_page + i),
                }),
            });
        }
    }
    doc
}

#[test]
fn export_does_not_hold_its_result_twice() {
    let doc = long_document(50, 200);
    let start = CUR.with(Cell::get);
    PEAK.with(|p| p.set(start));
    let json = doc.export_to_json_value();
    let peak = (PEAK.with(Cell::get) - start) as usize;
    let output = (CUR.with(Cell::get) - start) as usize;
    eprintln!("export peak {peak} B for a {output} B value");
    assert_eq!(json["texts"].as_array().map(Vec::len), Some(50 * 200));
    assert!(
        peak * 4 < output * 5,
        "export peaked at {peak} B for a {output} B value"
    );
}
