//! Writes a PBM preview of mixed Arabic/English receipt lines: `cargo run -p amwapos-core --example raster_preview -- out.pbm`
use amwapos_core::raster::{render, Bitmap, Place};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "raster_preview.pbm".into());
    let lines = [
        render(576, &[("سوبرماركت النور", Place::Center)], true, true),
        render(576, &[("Al Noor Supermarket — المنامة، البحرين", Place::Center)], false, false),
        render(576, &[("فاتورة ضريبية / TAX INVOICE", Place::Center)], true, false),
        render(576, &[("حليب المراعي طازج 1 لتر", Place::Left)], false, false),
        render(576, &[("  2 x 0.850", Place::Left), ("1.700", Place::Right)], false, false),
        render(576, &[("لبن لا دسم Laban 2L", Place::Left), ("0.650", Place::Right)], false, false),
        render(576, &[("الإجمالي TOTAL", Place::Left), ("BHD 2.350", Place::Right)], true, true),
        render(576, &[("شكراً لتسوقكم معنا", Place::Center)], false, false),
    ];
    let bm = Bitmap::stack(&lines);
    std::fs::write(&out, bm.to_pbm()).unwrap();
    println!("{out}: {}x{}", bm.width, bm.height);
}
