/// Built-in assets served from both the local stall host and the guest loopback
/// proxy (so the stage webview never has to fetch them over P2P).

pub fn stall_asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "/_stall/pdfjs/pdf.min.mjs" => Some((
            include_bytes!("vendor/pdfjs/pdf.min.mjs"),
            "text/javascript; charset=utf-8",
        )),
        "/_stall/pdfjs/pdf.worker.min.mjs" => Some((
            include_bytes!("vendor/pdfjs/pdf.worker.min.mjs"),
            "text/javascript; charset=utf-8",
        )),
        _ => None,
    }
}
