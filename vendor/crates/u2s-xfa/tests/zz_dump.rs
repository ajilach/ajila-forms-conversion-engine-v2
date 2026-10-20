//! Scratch helper: dumps a PDF's XFA packets. Delete before committing.
#[test]
#[ignore]
fn dump_xdp() {
    let src = std::env::var("DUMP_PDF").expect("DUMP_PDF");
    let out = std::env::var("DUMP_OUT").expect("DUMP_OUT");
    let bytes = std::fs::read(&src).unwrap();
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes).unwrap().expect("no xfa");
    std::fs::write(&out, xfa).unwrap();
}
